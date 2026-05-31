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

#include <switch.h>
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

// Standard audren config: 5ms revision (47 audio frames / sec).
constexpr AudioRendererConfig s_audren_cfg = {
    .output_rate    = AudioRendererOutputRate_48kHz,
    .num_voices     = 4,
    .num_effects    = 0,
    .num_sinks      = 1,
    .num_mix_objs   = 1,
    .num_mix_buffers = NUM_CHANNELS,
};

Thread s_worker_thread;
volatile bool s_worker_running = false;
volatile bool s_voice_should_play = true;
bool s_audio_initialised = false;

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

        // Walk each wave buffer; whenever one is Free or Done it's safe
        // to refill and resubmit.
        for (int i = 0; i < NUM_WAVE_BUFS; ++i) {
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
        if (s_voice_should_play && !audrvVoiceIsPlaying(&s_drv, 0)) {
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

extern "C" int ruffle_audio_init(unsigned int /*sample_rate*/, unsigned int /*channels*/) {
    if (s_audio_initialised) {
        return 0;
    }

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
    s_voice_should_play = true;

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

extern "C" void ruffle_audio_shutdown() {
    if (!s_audio_initialised) {
        return;
    }
    s_worker_running = false;
    threadWaitForExit(&s_worker_thread);
    threadClose(&s_worker_thread);
    audrvVoiceStop(&s_drv, 0);
    audrvUpdate(&s_drv);
    audrvClose(&s_drv);
    audrenExit();
    s_audio_initialised = false;
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
