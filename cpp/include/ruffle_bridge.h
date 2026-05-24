#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

int  ruffle_init(void);
void ruffle_render_frame(void);
/// Same as `ruffle_render_frame`, but the tick uses the supplied real elapsed
/// dt (microseconds) instead of the hardcoded 1/60 fallback. Lets Ruffle's
/// internal frame accumulator pace AS2 execution at the SWF's declared rate.
void ruffle_render_frame_dt(uint64_t dt_us);
void ruffle_shutdown(void);

/// Process RAM telemetry. `used_out` / `total_out` are filled via svcGetInfo
/// (InfoType_UsedMemorySize / InfoType_TotalMemorySize, CUR_PROCESS_HANDLE).
/// Returns 0 on success, non-zero on failure.
int ruffle_query_ram(uint64_t* used_out, uint64_t* total_out);

/// Monotonic tick counter and its frequency (Hz). Used by the Rust side
/// to time bitmap decodes etc. without depending on stdlib clocks.
uint64_t ruffle_tick_now(void);
uint64_t ruffle_tick_freq(void);

#ifdef __cplusplus
}
#endif
