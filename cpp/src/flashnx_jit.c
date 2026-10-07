// FlashNX: code memory for the AVM2 JIT of our Ruffle
// (third_party/ruffle/core/src/avm2/activation_jit.rs).
//
// libnx's jitCreate picks CodeMemory when the loader hints svcCreateCodeMemory
// and svcControlCodeMemory, which hbloader does on Mesosphère (it detects
// same-process code memory, see nx-hbloader's getCodeMemoryCapability). That
// type keeps a writable and an executable view of the same pages mapped at
// once, so code is written once and only the caches need syncing. The other
// type (svcSetProcessMemoryPermission) unmaps the executable view on every
// write, under code that may be running: not supported, the JIT stays off.

#include <switch.h>

static Jit g_jit;
static int g_state; // 0 = not tried, 1 = ready, -1 = unavailable
static u32 g_rc;

u32 flashnx_jit_init(size_t size, void** rw, void** rx)
{
    if (g_state == 0) {
        g_state = -1;
        Result rc = jitCreate(&g_jit, size);
        if (R_SUCCEEDED(rc) && g_jit.type != JitType_CodeMemory) {
            jitClose(&g_jit);
            rc = 0xFFFF;
        }
        if (R_SUCCEEDED(rc)) {
            // CodeMemory: only flushes the caches, the views stay mapped.
            rc = jitTransitionToExecutable(&g_jit);
        }
        g_rc = rc;
        if (R_SUCCEEDED(rc))
            g_state = 1;
    }
    if (g_state != 1)
        return g_rc ? g_rc : 0xFFFE;
    *rw = g_jit.rw_addr;
    *rx = g_jit.rx_addr;
    return 0;
}

// New code was written at rw; make it visible to instruction fetch at rx.
void flashnx_jit_flush(void* rw, void* rx, size_t len)
{
    armDCacheFlush(rw, len);
    armICacheInvalidate(rx, len);
}
