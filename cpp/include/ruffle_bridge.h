#pragma once

#ifdef __cplusplus
extern "C" {
#endif

int  ruffle_init(void);
void ruffle_render_frame(void);
void ruffle_shutdown(void);

#ifdef __cplusplus
}
#endif
