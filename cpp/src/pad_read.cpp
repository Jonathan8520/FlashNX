#include "pad_read.h"

#include <cstdio>

// See pad_read.h. The reading itself is libnx's `padUpdate` (nx/source/runtime/
// pad.c): same sources, same order of styles, same rule for the sticks (the
// first one moving wins). Only the lone Joy-Cons are turned on the way.

namespace {

struct Move {
    u64 from;
    u64 to;
};

// The left Joy-Con on its side, stick to the left: a quarter turn
// anticlockwise from upright. Its arrow buttons are the face buttons, the one
// on the right being A, as on the left Joy-Con picture in Nintendo's solo
// grip; its stick's up is the side of the rail.
const Move LEFT_MOVES[] = {
    {HidNpadButton_Down, HidNpadButton_A},
    {HidNpadButton_Left, HidNpadButton_B},
    {HidNpadButton_Right, HidNpadButton_X},
    {HidNpadButton_Up, HidNpadButton_Y},
    {HidNpadButton_Minus, HidNpadButton_Minus},
    {HidNpadButton_LeftSL, HidNpadButton_LeftSL},
    {HidNpadButton_LeftSR, HidNpadButton_LeftSR},
    {HidNpadButton_StickL, HidNpadButton_StickL},
    {HidNpadButton_StickLUp, HidNpadButton_StickLLeft},
    {HidNpadButton_StickLRight, HidNpadButton_StickLUp},
    {HidNpadButton_StickLDown, HidNpadButton_StickLRight},
    {HidNpadButton_StickLLeft, HidNpadButton_StickLDown},
};

// The right Joy-Con on its side, stick to the left: a quarter turn clockwise.
// Its stick becomes the left stick, the only one a lone Joy-Con has. Its + is
// its small button: player 1's opens the pause (`PAD_FLAG_PLAYER1`).
const Move RIGHT_MOVES[] = {
    {HidNpadButton_X, HidNpadButton_A},
    {HidNpadButton_A, HidNpadButton_B},
    {HidNpadButton_Y, HidNpadButton_X},
    {HidNpadButton_B, HidNpadButton_Y},
    {HidNpadButton_Plus, HidNpadButton_Plus},
    {HidNpadButton_RightSL, HidNpadButton_RightSL},
    {HidNpadButton_RightSR, HidNpadButton_RightSR},
    {HidNpadButton_StickR, HidNpadButton_StickL},
    {HidNpadButton_StickRDown, HidNpadButton_StickLLeft},
    {HidNpadButton_StickRLeft, HidNpadButton_StickLUp},
    {HidNpadButton_StickRUp, HidNpadButton_StickLRight},
    {HidNpadButton_StickRRight, HidNpadButton_StickLDown},
};

bool stick_moving(const HidAnalogStickState& s) {
    return s.x != 0 || s.y != 0;
}

void combine(PadState* pad, const HidNpadCommonState& st) {
    pad->attributes |= st.attributes;
    pad->buttons_cur |= st.buttons;
    if (!stick_moving(pad->sticks[0])) pad->sticks[0] = st.analog_stick_l;
    if (!stick_moving(pad->sticks[1])) pad->sticks[1] = st.analog_stick_r;
}

// One lone Joy-Con's state, turned to a standard pad. Its L / ZL (or R / ZR),
// on the far end of a Joy-Con held on its side, are left out, as Nintendo's
// solo grip, SDL and RetroArch leave them.
void turn(bool left, u32 flags, HidNpadCommonState& st) {
    const Move* moves = left ? LEFT_MOVES : RIGHT_MOVES;
    const size_t n = left ? sizeof(LEFT_MOVES) / sizeof(LEFT_MOVES[0])
                          : sizeof(RIGHT_MOVES) / sizeof(RIGHT_MOVES[0]);
    u64 out = 0;
    for (size_t i = 0; i < n; ++i) {
        if (st.buttons & moves[i].from) out |= moves[i].to;
    }
    if (!left && (flags & PAD_FLAG_PLAYER1) && (out & HidNpadButton_Plus)) {
        out = (out & ~HidNpadButton_Plus) | HidNpadButton_Minus;
    }
    if (flags & PAD_FLAG_SHOULDERS) out = pad_lone_shoulders(out);
    st.buttons = out;
    // The same quarter turn as the arrows above, in numbers (y up).
    const HidAnalogStickState raw = left ? st.analog_stick_l : st.analog_stick_r;
    if (left) {
        st.analog_stick_l = HidAnalogStickState{-raw.y, raw.x};
    } else {
        st.analog_stick_l = HidAnalogStickState{raw.y, -raw.x};
    }
    st.analog_stick_r = HidAnalogStickState{0, 0};
}

}  // namespace

u32 flashnx_pad_update(PadState* pad, u32 flags, bool log) {
    // What each controller id's lone Joy-Con sent last, for `log`.
    static u64 s_last_raw[8] = {};
    u32 read = 0;
    pad->active_id_mask = 0;
    pad->active_handheld = false;
    pad->style_set = 0;
    pad->attributes = 0;
    pad->buttons_old = pad->buttons_cur;
    pad->buttons_cur = 0;
    pad->sticks[0] = pad->sticks[1] = HidAnalogStickState{0, 0};
    pad->gc_triggers[0] = pad->gc_triggers[1] = 0;

    if (pad->read_handheld) {
        HidNpadCommonState state;
        if (hidGetNpadStatesHandheld(HidNpadIdType_Handheld, &state, 1) &&
            (state.attributes & HidNpadAttribute_IsConnected)) {
            pad->active_handheld = true;
            pad->style_set |= HidNpadStyleTag_NpadHandheld;
            combine(pad, state);
        }
    }

    u32 id_mask = pad->id_mask;
    while (id_mask) {
        const u32 i = __builtin_ffs(id_mask) - 1;
        const HidNpadIdType id = (HidNpadIdType)(HidNpadIdType_No1 + i);
        id_mask &= ~BIT(i);

        const u32 style_set = hidGetNpadStyleSet(id);
        if (style_set == 0) continue;

        if (style_set & HidNpadStyleTag_NpadGc) {
            HidNpadGcState state;
            if (hidGetNpadStatesGc(id, &state, 1) && (state.attributes & HidNpadAttribute_IsConnected)) {
                pad->active_id_mask |= BIT(i);
                pad->style_set |= style_set;
                pad->attributes |= state.attributes;
                pad->buttons_cur |= state.buttons;
                if (!stick_moving(pad->sticks[0])) pad->sticks[0] = state.analog_stick_l;
                if (!stick_moving(pad->sticks[1])) pad->sticks[1] = state.analog_stick_r;
                if (pad->gc_triggers[0] < state.trigger_l) pad->gc_triggers[0] = state.trigger_l;
                if (pad->gc_triggers[1] < state.trigger_r) pad->gc_triggers[1] = state.trigger_r;
            }
            continue;
        }

        HidNpadCommonState state;
        size_t count = 0;
        int lone = 0;  // 1 left Joy-Con on its own, 2 right
        if (style_set & HidNpadStyleTag_NpadSystemExt) {
            count = hidGetNpadStatesSystemExt(id, &state, 1);
        } else if (style_set & HidNpadStyleTag_NpadFullKey) {
            count = hidGetNpadStatesFullKey(id, &state, 1);
        } else if (style_set & HidNpadStyleTag_NpadJoyDual) {
            count = hidGetNpadStatesJoyDual(id, &state, 1);
        } else if (style_set & HidNpadStyleTag_NpadJoyLeft) {
            count = hidGetNpadStatesJoyLeft(id, &state, 1);
            lone = 1;
        } else if (style_set & HidNpadStyleTag_NpadJoyRight) {
            count = hidGetNpadStatesJoyRight(id, &state, 1);
            lone = 2;
        }
        if (count != 0 && (state.attributes & HidNpadAttribute_IsConnected)) {
            if (lone) {
                const u64 raw = state.buttons;
                turn(lone == 1, flags, state);
                read |= PAD_READ_SINGLE;
                if (log && raw != s_last_raw[i]) {
                    s_last_raw[i] = raw;
                    std::printf("joycon: No%u %s raw 0x%llx -> 0x%llx\n", i + 1, lone == 1 ? "left" : "right",
                                (unsigned long long)raw, (unsigned long long)state.buttons);
                    std::fflush(stdout);
                }
            }
            pad->active_id_mask |= BIT(i);
            pad->style_set |= style_set;
            combine(pad, state);
        }
    }
    return read;
}

extern "C" int flashnx_player_pad_kind(int player) {
    if (player < 1 || player > 4) return 0;
    const HidNpadIdType id = (HidNpadIdType)(HidNpadIdType_No1 + player - 1);
    const u32 style = hidGetNpadStyleSet(id);
    if (style & (HidNpadStyleTag_NpadFullKey | HidNpadStyleTag_NpadJoyDual | HidNpadStyleTag_NpadSystemExt |
                 HidNpadStyleTag_NpadGc)) {
        return 1;
    }
    if (style & HidNpadStyleTag_NpadJoyLeft) return 2;
    if (style & HidNpadStyleTag_NpadJoyRight) return 3;
    if (style != 0) return 1;
    // Player 1 also plays on the console in hand.
    if (player == 1 && hidGetNpadStyleSet(HidNpadIdType_Handheld) != 0) return 1;
    return 0;
}

extern "C" int flashnx_controller_applet(int max_players) {
    HidLaControllerSupportArg arg;
    hidLaCreateControllerSupportArg(&arg);
    // Every player FlashNX reads, no fewer required: the screen is opened to
    // hand controllers out, and closing it as it came changes nothing.
    arg.hdr.player_count_min = 0;
    arg.hdr.player_count_max = (s8)max_players;
    HidLaControllerSupportResultInfo info = {};
    const Result rc = hidLaShowControllerSupport(&info, &arg);
    std::printf("controllers: screen closed rc=0x%x players=%d (max %d)\n", rc, (int)info.player_count, max_players);
    std::fflush(stdout);
    return R_SUCCEEDED(rc) ? (int)info.player_count : -1;
}
