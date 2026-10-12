// SwitchAudioBackend C++ side — wraps libnx audren so the Rust-side
// AudioMixer (Ruffle's software audio mixer) can output to the speakers.
//
// Architecture (mirrors `frontend-utils/src/backends/audio.rs` CpalAudioBackend):
//   - `ruffle_audio_init` brings audren up + spawns a worker thread.
//   - The worker thread loops on `audrenWaitFrame()` and, whenever a wave
//     buffer is Free or Done, refills it with `ruffle_audio_fill_buffer`
//     (calls into the Rust mixer proxy) and resubmits via
//     `audrvVoiceAddWaveBuf`.
//   - `ruffle_audio_play`/`pause` only gate whether the voice is playing;
//     the worker keeps filling buffers either way.
//
// Format: 48 kHz stereo PCM int16 (Switch native, matches what we tell
// Ruffle's AudioMixer in SwitchAudioBackend::new). Two ping-pong wave buffers
// of 4096 frames each (~85 ms latency at 48 kHz).
//
// The interface sounds (the menus' clicks) live here too, on voices 1-3:
// short sounds synthesised once into their own memory pool and started by
// the worker within one audren frame of being asked for, so a click lands
// with the press instead of behind the ~340 ms of game audio queued on voice
// 0. In the launcher no game owns audren, so the first sound brings it up on
// its own ("UI mode", voice 0 left idle); a game starting adopts that session
// and its end tears it down as before.

#include <switch.h>
#include <atomic>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <cstdint>

namespace {

constexpr int    SAMPLE_RATE      = 48000;
constexpr int    NUM_CHANNELS     = 2;
constexpr int    FRAMES_PER_BUF   = 4096;
constexpr int    SAMPLES_PER_BUF  = FRAMES_PER_BUF * NUM_CHANNELS;   // i16 count
constexpr size_t BYTES_PER_BUF    = SAMPLES_PER_BUF * sizeof(int16_t);
// 4 wave buffers = ~340ms of cushion. The Rust mixer takes a Mutex on
// sound_instances during `mix`; whenever Ruffle starts a new sound from
// AS2 (Mario 63 plays many short SFX), the main thread holds that lock
// briefly and stalls our audio worker. 2 buffers (~170ms) wasn't enough
// to absorb those stalls and produced occasional crackles. 4 doubles the
// cushion with negligible added latency.
constexpr int    NUM_WAVE_BUFS    = 4;
// Round up to AUDREN_MEMPOOL_ALIGNMENT (page-sized) for the mempool.
constexpr size_t MEMPOOL_SIZE     = ((BYTES_PER_BUF * NUM_WAVE_BUFS)
                                     + AUDREN_MEMPOOL_ALIGNMENT - 1)
                                    & ~(AUDREN_MEMPOOL_ALIGNMENT - 1);

// Audren needs the memory pool aligned to AUDREN_MEMPOOL_ALIGNMENT (0x1000
// per libnx). We use a static aligned buffer rather than aligned_alloc to
// avoid newlib quirks.
alignas(AUDREN_MEMPOOL_ALIGNMENT) uint8_t s_mempool[128 * 1024];
static_assert(sizeof(s_mempool) >= MEMPOOL_SIZE,
              "s_mempool too small for the configured wave buffers");

AudioDriver       s_drv;
AudioDriverWaveBuf s_wavebufs[NUM_WAVE_BUFS];

// Standard audren config: 5ms revision (47 audio frames / sec). Six voices
// because audren hands out as many CHANNELS as voices: the game's stereo voice
// takes two, the three mono interface voices one each.
constexpr AudioRendererConfig s_audren_cfg = {
    .output_rate    = AudioRendererOutputRate_48kHz,
    .num_voices     = 6,
    .num_effects    = 0,
    .num_sinks      = 1,
    .num_mix_objs   = 1,
    .num_mix_buffers = NUM_CHANNELS,
};

Thread s_worker_thread;
volatile bool s_worker_running = false;
volatile bool s_voice_should_play = true;
bool s_audio_initialised = false;
// Voice 0 is fed by Ruffle's mixer only while a game owns the session. In UI
// mode there is no mixer to pull from: the fill would leave stale samples.
volatile bool s_game_mode = false;

// ── Interface sounds ───────────────────────────────────────────────────────
// Kinds, in the order `ruffle_ui_sound` takes them (rust/src/sfx.rs).
enum UiSound {
    UI_MOVE = 0,   // the cursor moved
    UI_DECIDE,     // A opened or did something
    UI_BACK,       // B went back
    UI_TAB,        // L / R changed tab
    UI_TOGGLE,     // a value changed in place
    UI_ERROR,      // refused, or failed
    UI_OPEN,       // a window or the pause opened
    UI_CLOSE,      // it closed
    UI_LAUNCH,     // a game is starting
    UI_COUNT,
};
constexpr int UI_FIRST_VOICE = 1;
constexpr int UI_VOICES      = 3;
constexpr int SFX_RATE       = 48000;
constexpr int SFX_CAPACITY   = 64 * 1024;            // mono samples
alignas(AUDREN_MEMPOOL_ALIGNMENT) int16_t s_sfx_pool[SFX_CAPACITY];
struct SfxSlot { int offset; int frames; };
SfxSlot s_sfx[UI_COUNT];
bool s_sfx_built = false;
AudioDriverWaveBuf s_sfx_wavebufs[UI_VOICES];
bool s_sfx_voice_ok[UI_VOICES];
int s_sfx_next_voice = 0;
std::atomic<int> s_sfx_request{0};    // kind + 1, 0 = nothing asked
volatile bool s_ui_sounds_enabled = true;
// Scratch for the sound being synthesised (the longest is ~0.35 s).
float s_sfx_scratch[20000];

// One note into the scratch: a sine gliding from `f0` to `f1` over `len`
// samples starting at `at`, a quick linear attack, an exponential decay, an
// optional octave (`h2`) and third harmonic (`h3`) for body.
void sfx_tone(int at, int len, float f0, float f1, float amp, float attack_ms,
              float decay_ms, float h2, float h3) {
    const float two_pi = 6.28318530718f;
    const int attack = (int)(attack_ms * SFX_RATE / 1000.0f) + 1;
    const float tau = decay_ms * SFX_RATE / 1000.0f;
    float phase = 0.0f;
    for (int i = 0; i < len; ++i) {
        const int k = at + i;
        if (k < 0 || k >= (int)(sizeof(s_sfx_scratch) / sizeof(float))) break;
        const float t = (float)i / (float)len;
        const float f = f0 + (f1 - f0) * t;
        phase += two_pi * f / SFX_RATE;
        if (phase > two_pi) phase -= two_pi;
        float env = i < attack ? (float)i / (float)attack : std::exp(-(float)(i - attack) / tau);
        // A short fade at the very end, so a cut never clicks.
        const int tail = len - i;
        if (tail < 96) env *= (float)tail / 96.0f;
        const float v = std::sin(phase) + h2 * std::sin(2.0f * phase) + h3 * std::sin(3.0f * phase);
        s_sfx_scratch[k] += amp * env * v;
    }
}

// Commit the scratch (its first `frames` samples) as sound `kind`.
void sfx_commit(int kind, int& cursor, int frames) {
    // Each sound starts on a 64-sample boundary: audren refuses a wave buffer
    // whose data is not aligned.
    cursor = (cursor + 63) & ~63;
    if (cursor + frames > SFX_CAPACITY) frames = SFX_CAPACITY - cursor;
    for (int i = 0; i < frames; ++i) {
        float v = s_sfx_scratch[i];
        // Soft clip, in case notes overlap past full scale.
        v = v / (1.0f + std::fabs(v) * 0.15f);
        if (v > 1.0f) v = 1.0f;
        if (v < -1.0f) v = -1.0f;
        s_sfx_pool[cursor + i] = (int16_t)(v * 32000.0f);
    }
    s_sfx[kind] = { cursor, frames };
    cursor += frames;
}

void sfx_build() {
    if (s_sfx_built) return;
    int cursor = 0;
    auto clear = [] { std::memset(s_sfx_scratch, 0, sizeof(s_sfx_scratch)); };
    // A soft tick, high and very short: it plays on every step of a list.
    clear(); sfx_tone(0, 1400, 1500, 1400, 0.20f, 0.6f, 6.0f, 0.25f, 0.0f);
    sfx_commit(UI_MOVE, cursor, 1400);
    // Two notes up a fifth: yes.
    clear(); sfx_tone(0, 2600, 988, 988, 0.22f, 1.0f, 22.0f, 0.20f, 0.0f);
    sfx_tone(2000, 3600, 1480, 1480, 0.22f, 1.0f, 35.0f, 0.20f, 0.0f);
    sfx_commit(UI_DECIDE, cursor, 5600);
    // The same, coming down a fourth: back.
    clear(); sfx_tone(0, 2400, 1175, 1175, 0.20f, 1.0f, 20.0f, 0.20f, 0.0f);
    sfx_tone(1900, 3100, 784, 784, 0.20f, 1.0f, 32.0f, 0.20f, 0.0f);
    sfx_commit(UI_BACK, cursor, 5000);
    // A small rising sweep: sideways, to the next tab.
    clear(); sfx_tone(0, 2600, 700, 1050, 0.18f, 2.0f, 16.0f, 0.15f, 0.0f);
    sfx_commit(UI_TAB, cursor, 2600);
    // An even shorter, higher tick: a value changed.
    clear(); sfx_tone(0, 1200, 2100, 2000, 0.16f, 0.5f, 4.5f, 0.20f, 0.0f);
    sfx_commit(UI_TOGGLE, cursor, 1200);
    // Two low, buzzy pulses: no.
    clear(); sfx_tone(0, 3400, 233, 220, 0.22f, 2.0f, 45.0f, 0.0f, 0.35f);
    sfx_tone(3600, 4400, 233, 220, 0.22f, 2.0f, 45.0f, 0.0f, 0.35f);
    sfx_commit(UI_ERROR, cursor, 8000);
    // A sweep up an octave, and its mirror: a window opens, it closes.
    clear(); sfx_tone(0, 4300, 520, 1040, 0.16f, 4.0f, 30.0f, 0.10f, 0.0f);
    sfx_commit(UI_OPEN, cursor, 4300);
    clear(); sfx_tone(0, 3800, 1000, 520, 0.16f, 3.0f, 26.0f, 0.10f, 0.0f);
    sfx_commit(UI_CLOSE, cursor, 3800);
    // C E G C, ringing into each other: a game is starting.
    clear();
    sfx_tone(0, 9000, 1046.5f, 1046.5f, 0.17f, 2.0f, 90.0f, 0.20f, 0.0f);
    sfx_tone(2400, 9000, 1318.5f, 1318.5f, 0.17f, 2.0f, 90.0f, 0.20f, 0.0f);
    sfx_tone(4800, 9000, 1568.0f, 1568.0f, 0.17f, 2.0f, 90.0f, 0.20f, 0.0f);
    sfx_tone(7200, 8800, 2093.0f, 2093.0f, 0.17f, 2.0f, 110.0f, 0.20f, 0.0f);
    sfx_commit(UI_LAUNCH, cursor, 16000);
    armDCacheFlush(s_sfx_pool, sizeof(s_sfx_pool));
    s_sfx_built = true;
    std::printf("audio: %d interface sounds, %d samples\n", (int)UI_COUNT, cursor);
    std::fflush(stdout);
}

// On the worker thread only: every audrv call happens there, so the driver
// is never touched from two threads at once.
void sfx_play(int kind) {
    if (kind < 0 || kind >= UI_COUNT || s_sfx[kind].frames <= 0) return;
    // Round robin over the voices that came up, so a launch chord is not cut
    // by the tick that follows it.
    int slot = -1;
    for (int n = 0; n < UI_VOICES; ++n) {
        const int cand = (s_sfx_next_voice + n) % UI_VOICES;
        if (s_sfx_voice_ok[cand]) {
            slot = cand;
            break;
        }
    }
    if (slot < 0) return;
    s_sfx_next_voice = (slot + 1) % UI_VOICES;
    const int voice = UI_FIRST_VOICE + slot;
    audrvVoiceStop(&s_drv, voice);
    AudioDriverWaveBuf* wb = &s_sfx_wavebufs[slot];
    std::memset(wb, 0, sizeof(*wb));
    wb->data_pcm16 = s_sfx_pool + s_sfx[kind].offset;
    wb->size = (u64)s_sfx[kind].frames * sizeof(int16_t);
    wb->start_sample_offset = 0;
    wb->end_sample_offset = s_sfx[kind].frames;
    wb->state = AudioDriverWaveBufState_Free;
    if (audrvVoiceAddWaveBuf(&s_drv, voice, wb)) {
        audrvVoiceStart(&s_drv, voice);
    }
}

extern "C" void ruffle_audio_fill_buffer(int16_t* out, size_t len);

void worker_entry(void* /*arg*/) {
    std::printf("audio worker: starting on core %d\n",
                svcGetCurrentProcessorNumber()); std::fflush(stdout);
    uint32_t fills = 0;
    uint32_t fills_with_signal = 0;
    uint32_t underruns = 0;
    int16_t max_abs_seen = 0;

    while (s_worker_running) {
        // Block until audren signals the next render frame (~5 ms).
        audrenWaitFrame();

        // An interface sound asked for since the last frame.
        const int req = s_sfx_request.exchange(0);
        if (req > 0) sfx_play(req - 1);

        // Walk each wave buffer; whenever one is Free or Done it's safe
        // to refill and resubmit. Only while a game feeds voice 0.
        for (int i = 0; i < NUM_WAVE_BUFS && s_game_mode; ++i) {
            AudioDriverWaveBuf* wb = &s_wavebufs[i];
            if (wb->state != AudioDriverWaveBufState_Free
                && wb->state != AudioDriverWaveBufState_Done) {
                continue;
            }

            // Pull fresh samples from the Rust mixer. fill_buffer fills
            // `SAMPLES_PER_BUF` interleaved i16 samples (stereo so the
            // frame count is SAMPLES_PER_BUF / NUM_CHANNELS).
            int16_t* data = wb->data_pcm16;
            ruffle_audio_fill_buffer(data, SAMPLES_PER_BUF);

            // Diagnostic: scan max abs sample value so we can distinguish
            // "mixer returns silence" from "audren config wrong".
            int16_t local_max = 0;
            for (int s = 0; s < SAMPLES_PER_BUF; ++s) {
                int16_t v = data[s];
                if (v < 0) v = -v;
                if (v > local_max) local_max = v;
            }
            if (local_max > max_abs_seen) max_abs_seen = local_max;
            if (local_max > 0) fills_with_signal++;
            fills++;
            // Log every ~5 sec (= ~625 buffers if ~85 ms/buf).
            if (fills % 60 == 0) {
                std::printf(
                    "audio: fills=%u with_signal=%u max_seen=%d underruns=%u voice_playing=%d\n",
                    fills, fills_with_signal, (int)max_abs_seen, underruns,
                    audrvVoiceIsPlaying(&s_drv, 0));
                std::fflush(stdout);
            }

            // Flush CPU cache so the audio renderer sees the new samples.
            armDCacheFlush(data, BYTES_PER_BUF);

            // Reset the wave buffer so audren will play this segment.
            wb->start_sample_offset = 0;
            wb->end_sample_offset   = FRAMES_PER_BUF;
            audrvVoiceAddWaveBuf(&s_drv, 0, wb);
        }

        // Tick audren so it consumes our submissions. Also (re)start the
        // voice if it stopped — happens on the very first frame and after
        // an underrun.
        audrvUpdate(&s_drv);
        if (s_game_mode && s_voice_should_play && !audrvVoiceIsPlaying(&s_drv, 0)) {
            // Voice stopped while it should play = buffers ran dry (underrun).
            // Count it past warm-up so the heartbeat shows whether crackles
            // correlate with CPU-starved refills.
            if (fills > (uint32_t)NUM_WAVE_BUFS) underruns++;
            audrvVoiceStart(&s_drv, 0);
        }
    }

    std::printf("audio worker: exiting (fills=%u with_signal=%u max=%d underruns=%u)\n",
                fills, fills_with_signal, (int)max_abs_seen, underruns);
    std::fflush(stdout);
}

} // namespace

namespace {

// Bring audren up: the session, the game's voice 0 (left idle until a game
// feeds it), the interface voices, and the worker. Shared by the game's start
// and by the first interface sound in the launcher.
int audio_bring_up() {
    sfx_build();
    Result rc = audrenInitialize(&s_audren_cfg);
    if (R_FAILED(rc)) {
        std::printf("audrenInitialize failed: 0x%x\n", rc); std::fflush(stdout);
        return -1;
    }
    rc = audrvCreate(&s_drv, &s_audren_cfg, NUM_CHANNELS);
    if (R_FAILED(rc)) {
        std::printf("audrvCreate failed: 0x%x\n", rc); std::fflush(stdout);
        audrenExit();
        return -1;
    }

    // Register the wave-buffer memory pool with audren.
    int mpid = audrvMemPoolAdd(&s_drv, s_mempool, MEMPOOL_SIZE);
    if (mpid < 0) {
        std::printf("audrvMemPoolAdd failed\n"); std::fflush(stdout);
        audrvClose(&s_drv);
        audrenExit();
        return -1;
    }
    if (!audrvMemPoolAttach(&s_drv, mpid)) {
        std::printf("audrvMemPoolAttach failed\n"); std::fflush(stdout);
        audrvClose(&s_drv);
        audrenExit();
        return -1;
    }

    // Default device sink: stereo output to the system speaker / headphones.
    static const u8 sink_channels[2] = { 0, 1 };
    audrvDeviceSinkAdd(&s_drv, AUDREN_DEFAULT_DEVICE_NAME, 2, sink_channels);

    rc = audrenStartAudioRenderer();
    if (R_FAILED(rc)) {
        std::printf("audrenStartAudioRenderer failed: 0x%x\n", rc); std::fflush(stdout);
        audrvClose(&s_drv);
        audrenExit();
        return -1;
    }

    // Voice 0 = our only voice. Send it to the final mix.
    if (!audrvVoiceInit(&s_drv, 0, NUM_CHANNELS, PcmFormat_Int16, SAMPLE_RATE)) {
        std::printf("audrvVoiceInit failed\n"); std::fflush(stdout);
        audrvClose(&s_drv);
        audrenExit();
        return -1;
    }
    audrvVoiceSetDestinationMix(&s_drv, 0, AUDREN_FINAL_MIX_ID);
    audrvVoiceSetMixFactor(&s_drv, 0, 1.0f, 0, 0);  // L source → L final
    audrvVoiceSetMixFactor(&s_drv, 0, 1.0f, 1, 1);  // R source → R final

    // The interface voices: mono, from their own pool, to both speakers.
    {
        for (int v = 0; v < UI_VOICES; ++v) s_sfx_voice_ok[v] = false;
        int sfx_pool = audrvMemPoolAdd(&s_drv, s_sfx_pool, sizeof(s_sfx_pool));
        if (sfx_pool >= 0 && audrvMemPoolAttach(&s_drv, sfx_pool)) {
            for (int v = 0; v < UI_VOICES; ++v) {
                const int id = UI_FIRST_VOICE + v;
                if (!audrvVoiceInit(&s_drv, id, 1, PcmFormat_Int16, SFX_RATE)) continue;
                s_sfx_voice_ok[v] = true;
                audrvVoiceSetDestinationMix(&s_drv, id, AUDREN_FINAL_MIX_ID);
                audrvVoiceSetMixFactor(&s_drv, id, 0.85f, 0, 0);
                audrvVoiceSetMixFactor(&s_drv, id, 0.85f, 0, 1);
            }
        } else {
            std::printf("audio: interface sounds unavailable (mempool)\n"); std::fflush(stdout);
        }
        std::memset(s_sfx_wavebufs, 0, sizeof(s_sfx_wavebufs));
        s_sfx_next_voice = 0;
    }

    // Lay out wave buffers within the memory pool.
    std::memset(s_wavebufs, 0, sizeof(s_wavebufs));
    for (int i = 0; i < NUM_WAVE_BUFS; ++i) {
        int16_t* data = reinterpret_cast<int16_t*>(s_mempool + i * BYTES_PER_BUF);
        // Pre-zero so the first playback frame doesn't hiss.
        std::memset(data, 0, BYTES_PER_BUF);
        s_wavebufs[i].data_pcm16        = data;
        s_wavebufs[i].size              = BYTES_PER_BUF;
        s_wavebufs[i].start_sample_offset = 0;
        s_wavebufs[i].end_sample_offset   = FRAMES_PER_BUF;
        s_wavebufs[i].state             = AudioDriverWaveBufState_Free;
    }

    s_worker_running    = true;

    // Spawn the worker on a modest 64 KB stack — it only does memcpy +
    // syscalls, no deep stacks expected.
    // Pin the worker to core 2, away from the main Ruffle thread: heavy AVM
    // ticks (Mario 63 dense scenes exceed 1 s/frame) saturate the main thread's
    // core, and on a shared core that starved this worker → audible crackle.
    // Fall back to the default core (-2) if core 2 isn't in our core mask.
    rc = threadCreate(&s_worker_thread, worker_entry, nullptr, nullptr,
                      64 * 1024, 0x2C, 2);
    if (R_FAILED(rc)) {
        std::printf("audio worker: core 2 unavailable (0x%x), using default core\n", rc);
        std::fflush(stdout);
        rc = threadCreate(&s_worker_thread, worker_entry, nullptr, nullptr,
                          64 * 1024, 0x2C, -2);
    }
    if (R_FAILED(rc)) {
        std::printf("audio worker threadCreate failed: 0x%x\n", rc); std::fflush(stdout);
        audrvClose(&s_drv);
        audrenExit();
        s_worker_running = false;
        return -1;
    }
    threadStart(&s_worker_thread);

    s_audio_initialised = true;
    std::printf("audio: audren up (%d Hz stereo i16, %d wavebufs × %d frames)\n",
                SAMPLE_RATE, NUM_WAVE_BUFS, FRAMES_PER_BUF);
    std::fflush(stdout);
    return 0;
}

} // namespace

extern "C" int ruffle_audio_init(unsigned int /*sample_rate*/, unsigned int /*channels*/) {
    // The launcher may already have brought the session up for its sounds:
    // the game adopts it.
    if (!s_audio_initialised && audio_bring_up() != 0) {
        return -1;
    }
    s_voice_should_play = true;
    s_game_mode = true;
    return 0;
}

// Play interface sound `kind` (see `UiSound`). Brings audren up if nothing
// has: in the launcher no game owns it. Returns at once; the worker starts
// the sound on its next frame.
extern "C" void ruffle_ui_sound(int kind) {
    if (!s_ui_sounds_enabled || kind < 0 || kind >= UI_COUNT) return;
    if (!s_audio_initialised && audio_bring_up() != 0) return;
    s_sfx_request.store(kind + 1);
}

// RÉGLAGES > interface sounds.
extern "C" void ruffle_ui_sounds_set_enabled(int on) {
    s_ui_sounds_enabled = on != 0;
}

extern "C" void ruffle_audio_shutdown() {
    if (!s_audio_initialised) {
        return;
    }
    s_worker_running = false;
    threadWaitForExit(&s_worker_thread);
    threadClose(&s_worker_thread);
    audrvVoiceStop(&s_drv, 0);
    for (int v = 0; v < UI_VOICES; ++v) {
        if (s_sfx_voice_ok[v]) audrvVoiceStop(&s_drv, UI_FIRST_VOICE + v);
    }
    audrvUpdate(&s_drv);
    audrvClose(&s_drv);
    audrenExit();
    s_audio_initialised = false;
    s_game_mode = false;
}

extern "C" void ruffle_audio_play() {
    s_voice_should_play = true;
}

extern "C" void ruffle_audio_pause() {
    s_voice_should_play = false;
    if (s_audio_initialised && audrvVoiceIsPlaying(&s_drv, 0)) {
        audrvVoiceStop(&s_drv, 0);
        audrvUpdate(&s_drv);
    }
}
