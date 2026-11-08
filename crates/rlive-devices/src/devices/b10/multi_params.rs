// Parameter ids of the Sampler's extended engine. Generated once; append new ones at the end.

use super::common::{FILTER_CIRCUITS, FILTER_SLOPES, FILTER_TYPES};
use crate::dsp::tempo::SyncRate;
use rlive_core::{ParamFlags, ParamInfo};

pub const REVERSE: u32 = 9;
pub const SNAP: u32 = 10;
pub const INTERPOLATION: u32 = 11;
pub const RAM_MODE: u32 = 12;
pub const CONST_POWER: u32 = 13;
pub const LAYER_SELECT: u32 = 14;
pub const SELECTOR: u32 = 15;
pub const ZONE_SHIFT: u32 = 16;
pub const V_DECAY: u32 = 17;
pub const V_SUSTAIN: u32 = 18;
pub const V_INITIAL: u32 = 19;
pub const V_PEAK: u32 = 20;
pub const V_A_SLOPE: u32 = 21;
pub const V_D_SLOPE: u32 = 22;
pub const V_R_SLOPE: u32 = 23;
pub const V_TIME_VEL: u32 = 24;
pub const V_LOOP: u32 = 25;
pub const V_LOOP_TIME: u32 = 26;
pub const V_REPEAT: u32 = 27;
pub const TIME: u32 = 28;
pub const TIME_KEY: u32 = 29;
pub const PAN_RND: u32 = 30;
pub const VOICES: u32 = 31;
pub const RETRIGGER: u32 = 32;
pub const DETUNE: u32 = 33;
pub const SPREAD: u32 = 34;
pub const GLIDE: u32 = 35;
pub const GLIDE_TIME: u32 = 36;
pub const PE_ON: u32 = 37;
pub const PE_AMOUNT: u32 = 38;
pub const PE_BASE: u32 = 39;
pub const PE_INITIAL: u32 = 39;
pub const PE_PEAK: u32 = 40;
pub const PE_SUSTAIN: u32 = 41;
pub const PE_END: u32 = 42;
pub const PE_ATTACK: u32 = 43;
pub const PE_DECAY: u32 = 44;
pub const PE_RELEASE: u32 = 45;
pub const PE_A_SLOPE: u32 = 46;
pub const PE_D_SLOPE: u32 = 47;
pub const PE_R_SLOPE: u32 = 48;
pub const PE_LOOP: u32 = 49;
pub const PE_LOOP_TIME: u32 = 50;
pub const PE_REPEAT: u32 = 51;
pub const PE_TIME_VEL: u32 = 52;
pub const OSC_ON: u32 = 53;
pub const OSC_TYPE: u32 = 54;
pub const OSC_MODE: u32 = 55;
pub const OSC_VOL: u32 = 56;
pub const OSC_VOL_VEL: u32 = 57;
pub const OSC_FIXED: u32 = 58;
pub const OSC_FREQ: u32 = 59;
pub const OSC_MULTI: u32 = 60;
pub const OSC_COARSE: u32 = 61;
pub const OSC_FINE: u32 = 62;
pub const OE_BASE: u32 = 63;
pub const OE_INITIAL: u32 = 63;
pub const OE_PEAK: u32 = 64;
pub const OE_SUSTAIN: u32 = 65;
pub const OE_END: u32 = 66;
pub const OE_ATTACK: u32 = 67;
pub const OE_DECAY: u32 = 68;
pub const OE_RELEASE: u32 = 69;
pub const OE_A_SLOPE: u32 = 70;
pub const OE_D_SLOPE: u32 = 71;
pub const OE_R_SLOPE: u32 = 72;
pub const OE_LOOP: u32 = 73;
pub const OE_LOOP_TIME: u32 = 74;
pub const OE_REPEAT: u32 = 75;
pub const OE_TIME_VEL: u32 = 76;
pub const F_ON: u32 = 77;
pub const F_TYPE: u32 = 78;
pub const F_CIRCUIT: u32 = 79;
pub const F_SLOPE: u32 = 80;
pub const F_FREQ: u32 = 81;
pub const F_RES: u32 = 82;
pub const F_DRIVE: u32 = 83;
pub const F_MORPH: u32 = 84;
pub const FE_ON: u32 = 85;
pub const FE_AMOUNT: u32 = 86;
pub const FE_BASE: u32 = 87;
pub const FE_INITIAL: u32 = 87;
pub const FE_PEAK: u32 = 88;
pub const FE_SUSTAIN: u32 = 89;
pub const FE_END: u32 = 90;
pub const FE_ATTACK: u32 = 91;
pub const FE_DECAY: u32 = 92;
pub const FE_RELEASE: u32 = 93;
pub const FE_A_SLOPE: u32 = 94;
pub const FE_D_SLOPE: u32 = 95;
pub const FE_R_SLOPE: u32 = 96;
pub const FE_LOOP: u32 = 97;
pub const FE_LOOP_TIME: u32 = 98;
pub const FE_REPEAT: u32 = 99;
pub const FE_TIME_VEL: u32 = 100;
pub const SHAPER_ON: u32 = 101;
pub const SHAPER_TYPE: u32 = 102;
pub const SHAPER_AMOUNT: u32 = 103;
pub const SHAPER_ORDER: u32 = 104;
pub const AE_BASE: u32 = 105;
pub const AE_INITIAL: u32 = 105;
pub const AE_PEAK: u32 = 106;
pub const AE_SUSTAIN: u32 = 107;
pub const AE_END: u32 = 108;
pub const AE_ATTACK: u32 = 109;
pub const AE_DECAY: u32 = 110;
pub const AE_RELEASE: u32 = 111;
pub const AE_A_SLOPE: u32 = 112;
pub const AE_D_SLOPE: u32 = 113;
pub const AE_R_SLOPE: u32 = 114;
pub const AE_LOOP: u32 = 115;
pub const AE_LOOP_TIME: u32 = 116;
pub const AE_REPEAT: u32 = 117;
pub const AE_TIME_VEL: u32 = 118;
pub const AE_DEST_A: u32 = 119;
pub const AE_DEST_B: u32 = 120;
pub const AE_AMT_A: u32 = 121;
pub const AE_AMT_B: u32 = 122;
pub const L1_ON: u32 = 123;
pub const L1_TYPE: u32 = 124;
pub const L1_SYNC: u32 = 125;
pub const L1_FREQ: u32 = 126;
pub const L1_BEATS: u32 = 127;
pub const L1_ATTACK: u32 = 128;
pub const L1_RETRIG: u32 = 129;
pub const L1_OFFSET: u32 = 130;
pub const L1_KEY: u32 = 131;
pub const L1_TO_VOL: u32 = 132;
pub const L1_TO_PAN: u32 = 133;
pub const L1_TO_FILTER: u32 = 134;
pub const L1_TO_PITCH: u32 = 135;
pub const L2_ON: u32 = 136;
pub const L2_TYPE: u32 = 137;
pub const L2_SYNC: u32 = 138;
pub const L2_FREQ: u32 = 139;
pub const L2_BEATS: u32 = 140;
pub const L2_ATTACK: u32 = 141;
pub const L2_RETRIG: u32 = 142;
pub const L2_OFFSET: u32 = 143;
pub const L2_KEY: u32 = 144;
pub const L2_STEREO: u32 = 145;
pub const L2_PHASE: u32 = 146;
pub const L2_SPIN: u32 = 147;
pub const L2_DEST_A: u32 = 148;
pub const L2_DEST_B: u32 = 149;
pub const L2_AMT_A: u32 = 150;
pub const L2_AMT_B: u32 = 151;
pub const L3_ON: u32 = 152;
pub const L3_TYPE: u32 = 153;
pub const L3_SYNC: u32 = 154;
pub const L3_FREQ: u32 = 155;
pub const L3_BEATS: u32 = 156;
pub const L3_ATTACK: u32 = 157;
pub const L3_RETRIG: u32 = 158;
pub const L3_OFFSET: u32 = 159;
pub const L3_KEY: u32 = 160;
pub const L3_STEREO: u32 = 161;
pub const L3_PHASE: u32 = 162;
pub const L3_SPIN: u32 = 163;
pub const L3_DEST_A: u32 = 164;
pub const L3_DEST_B: u32 = 165;
pub const L3_AMT_A: u32 = 166;
pub const L3_AMT_B: u32 = 167;
pub const M_KEY_DEST_A: u32 = 168;
pub const M_KEY_DEST_B: u32 = 169;
pub const M_KEY_AMT_A: u32 = 170;
pub const M_KEY_AMT_B: u32 = 171;
pub const M_VEL_DEST_A: u32 = 172;
pub const M_VEL_DEST_B: u32 = 173;
pub const M_VEL_AMT_A: u32 = 174;
pub const M_VEL_AMT_B: u32 = 175;
pub const M_REL_DEST_A: u32 = 176;
pub const M_REL_DEST_B: u32 = 177;
pub const M_REL_AMT_A: u32 = 178;
pub const M_REL_AMT_B: u32 = 179;
pub const M_AT_DEST_A: u32 = 180;
pub const M_AT_DEST_B: u32 = 181;
pub const M_AT_AMT_A: u32 = 182;
pub const M_AT_AMT_B: u32 = 183;
pub const M_MW_DEST_A: u32 = 184;
pub const M_MW_DEST_B: u32 = 185;
pub const M_MW_AMT_A: u32 = 186;
pub const M_MW_AMT_B: u32 = 187;
pub const M_FC_DEST_A: u32 = 188;
pub const M_FC_DEST_B: u32 = 189;
pub const M_FC_AMT_A: u32 = 190;
pub const M_FC_AMT_B: u32 = 191;
pub const M_PB_DEST_A: u32 = 192;
pub const M_PB_DEST_B: u32 = 193;
pub const M_PB_AMT_A: u32 = 194;
pub const M_PB_AMT_B: u32 = 195;
pub const PB_RANGE: u32 = 196;
pub const LAST_EXT: u32 = 196;

pub const ENV_LOOP: [&str; 5] = ["None", "Loop", "Beat", "Sync", "Trigger"];
pub const LFO_TYPES: [&str; 6] = [
    "Sine",
    "Square",
    "Triangle",
    "Saw Down",
    "Saw Up",
    "Sample and Hold",
];
pub const OSC_WAVES: [&str; 21] = [
    "Sine",
    "Half Sine",
    "Full Sine",
    "Triangle",
    "Saw Up",
    "Saw Down",
    "Square",
    "Pulse 25",
    "Pulse 12",
    "Sine Oct",
    "Sine Fifth",
    "Sine Odd",
    "Saw Fat",
    "Tri Round",
    "Staircase 4",
    "Staircase 8",
    "Ramp Exp",
    "Random",
    "Noise",
    "Warble",
    "Pulse Sine",
];
pub const MOD_DESTS: [&str; 29] = [
    "Off",
    "Volume",
    "Pan",
    "Pitch",
    "Filter Freq",
    "Filter Res",
    "Filter Morph",
    "Filter Drive",
    "Shaper Amount",
    "Osc Volume",
    "Osc Pitch",
    "Sample Start",
    "Sample End",
    "Loop Start",
    "Loop Length",
    "Crossfade",
    "LFO 1 Rate",
    "LFO 1 Depth",
    "LFO 2 Rate",
    "LFO 2 Depth",
    "LFO 3 Rate",
    "LFO 3 Depth",
    "Attack",
    "Decay",
    "Release",
    "Filter Env",
    "Pitch Env",
    "Aux Env",
    "Global Time",
];

/// Every parameter appended after the original Sampler's. The defaults are all neutral.
pub fn ext_params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::toggle(REVERSE, "Reverse", false),
        ParamInfo::toggle(SNAP, "Snap", false),
        ParamInfo::stepped(
            INTERPOLATION,
            "Interpolation",
            &["Normal", "Good", "Best"],
            0,
        ),
        ParamInfo::toggle(RAM_MODE, "RAM Mode", false),
        ParamInfo::toggle(CONST_POWER, "Constant Power Fade", true),
        ParamInfo::continuous(LAYER_SELECT, "Selected Layer", 0.0, 255.0, 0.0, "")
            .with_flags(ParamFlags::HIDDEN),
        ParamInfo::continuous(SELECTOR, "Sample Selector", 0.0, 127.0, 0.0, ""),
        ParamInfo::continuous(ZONE_SHIFT, "Key Zone Shift", -48.0, 48.0, 0.0, "st"),
        ParamInfo::skewed(V_DECAY, "Decay", 0.0, 60000.0, 600.0, "ms", 3.5),
        ParamInfo::continuous(V_SUSTAIN, "Sustain", 0.0, 1.0, 1.0, ""),
        ParamInfo::continuous(V_INITIAL, "Initial", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(V_PEAK, "Peak", 0.0, 1.0, 1.0, ""),
        ParamInfo::continuous(V_A_SLOPE, "Attack Slope", -1.0, 1.0, 0.0, ""),
        ParamInfo::continuous(V_D_SLOPE, "Decay Slope", -1.0, 1.0, 0.0, ""),
        ParamInfo::continuous(V_R_SLOPE, "Release Slope", -1.0, 1.0, 0.0, ""),
        ParamInfo::continuous(V_TIME_VEL, "Time < Vel", -100.0, 100.0, 0.0, "%"),
        ParamInfo::stepped(V_LOOP, "Loop", &ENV_LOOP, 0),
        ParamInfo::skewed(V_LOOP_TIME, "Loop Time", 0.0, 20000.0, 0.0, "ms", 3.5),
        ParamInfo::stepped(V_REPEAT, "Repeat", &SyncRate::LABELS, 12),
        ParamInfo::continuous(TIME, "Global Time", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(TIME_KEY, "Time < Key", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(PAN_RND, "Pan < Rnd", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(VOICES, "Voices", 1.0, 32.0, 16.0, ""),
        ParamInfo::toggle(RETRIGGER, "Retrigger", false),
        ParamInfo::continuous(DETUNE, "Detune", -100.0, 100.0, 0.0, "ct"),
        ParamInfo::continuous(SPREAD, "Spread", 0.0, 100.0, 0.0, "%"),
        ParamInfo::stepped(GLIDE, "Glide", &["Off", "Glide", "Portamento"], 0),
        ParamInfo::skewed(GLIDE_TIME, "Glide Time", 1.0, 5000.0, 50.0, "ms", 3.0),
        ParamInfo::toggle(PE_ON, "Pitch Env", false),
        ParamInfo::continuous(PE_AMOUNT, "Pitch Env Amount", -48.0, 48.0, 0.0, "st"),
        ParamInfo::continuous(PE_INITIAL, "Pitch Env Initial", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(PE_PEAK, "Pitch Env Peak", 0.0, 1.0, 1.0, ""),
        ParamInfo::continuous(PE_SUSTAIN, "Pitch Env Sustain", 0.0, 1.0, 1.0, ""),
        ParamInfo::continuous(PE_END, "Pitch Env End", 0.0, 1.0, 0.0, ""),
        ParamInfo::skewed(PE_ATTACK, "Pitch Env Attack", 0.0, 20000.0, 0.1, "ms", 3.5),
        ParamInfo::skewed(PE_DECAY, "Pitch Env Decay", 0.0, 20000.0, 500.0, "ms", 3.5),
        ParamInfo::skewed(
            PE_RELEASE,
            "Pitch Env Release",
            0.0,
            20000.0,
            100.0,
            "ms",
            3.5,
        ),
        ParamInfo::continuous(PE_A_SLOPE, "Pitch Env A Slope", -1.0, 1.0, 0.0, ""),
        ParamInfo::continuous(PE_D_SLOPE, "Pitch Env D Slope", -1.0, 1.0, 0.0, ""),
        ParamInfo::continuous(PE_R_SLOPE, "Pitch Env R Slope", -1.0, 1.0, 0.0, ""),
        ParamInfo::stepped(PE_LOOP, "Pitch Env Loop", &ENV_LOOP, 0),
        ParamInfo::skewed(
            PE_LOOP_TIME,
            "Pitch Env Loop Time",
            0.0,
            20000.0,
            0.0,
            "ms",
            3.5,
        ),
        ParamInfo::stepped(PE_REPEAT, "Pitch Env Repeat", &SyncRate::LABELS, 12),
        ParamInfo::continuous(PE_TIME_VEL, "Pitch Env Time < Vel", -100.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(OSC_ON, "Osc", false),
        ParamInfo::stepped(OSC_TYPE, "Osc Type", &OSC_WAVES, 0),
        ParamInfo::stepped(OSC_MODE, "Osc Mode", &["FM", "AM"], 0),
        ParamInfo::continuous(OSC_VOL, "Osc Volume", 0.0, 100.0, 50.0, "%"),
        ParamInfo::continuous(OSC_VOL_VEL, "Osc Vol < Vel", -100.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(OSC_FIXED, "Osc Fixed", false),
        ParamInfo::continuous(OSC_FREQ, "Osc Freq", 0.0, 10000.0, 100.0, "Hz"),
        ParamInfo::stepped(OSC_MULTI, "Osc Multi", &["0.01", "0.1", "1", "10"], 2),
        ParamInfo::continuous(OSC_COARSE, "Osc Coarse", 0.125, 48.0, 1.0, ""),
        ParamInfo::continuous(OSC_FINE, "Osc Fine", 0.0, 1000.0, 0.0, ""),
        ParamInfo::continuous(OE_INITIAL, "Osc Env Initial", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(OE_PEAK, "Osc Env Peak", 0.0, 1.0, 1.0, ""),
        ParamInfo::continuous(OE_SUSTAIN, "Osc Env Sustain", 0.0, 1.0, 1.0, ""),
        ParamInfo::continuous(OE_END, "Osc Env End", 0.0, 1.0, 0.0, ""),
        ParamInfo::skewed(OE_ATTACK, "Osc Env Attack", 0.0, 20000.0, 0.1, "ms", 3.5),
        ParamInfo::skewed(OE_DECAY, "Osc Env Decay", 0.0, 20000.0, 500.0, "ms", 3.5),
        ParamInfo::skewed(
            OE_RELEASE,
            "Osc Env Release",
            0.0,
            20000.0,
            100.0,
            "ms",
            3.5,
        ),
        ParamInfo::continuous(OE_A_SLOPE, "Osc Env A Slope", -1.0, 1.0, 0.0, ""),
        ParamInfo::continuous(OE_D_SLOPE, "Osc Env D Slope", -1.0, 1.0, 0.0, ""),
        ParamInfo::continuous(OE_R_SLOPE, "Osc Env R Slope", -1.0, 1.0, 0.0, ""),
        ParamInfo::stepped(OE_LOOP, "Osc Env Loop", &ENV_LOOP, 0),
        ParamInfo::skewed(
            OE_LOOP_TIME,
            "Osc Env Loop Time",
            0.0,
            20000.0,
            0.0,
            "ms",
            3.5,
        ),
        ParamInfo::stepped(OE_REPEAT, "Osc Env Repeat", &SyncRate::LABELS, 12),
        ParamInfo::continuous(OE_TIME_VEL, "Osc Env Time < Vel", -100.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(F_ON, "Filter", false),
        ParamInfo::stepped(F_TYPE, "Filter Type", &FILTER_TYPES, 0),
        ParamInfo::stepped(F_CIRCUIT, "Circuit", &FILTER_CIRCUITS, 0),
        ParamInfo::stepped(F_SLOPE, "Slope", &FILTER_SLOPES, 0),
        ParamInfo::skewed(F_FREQ, "Frequency", 20.0, 20000.0, 2000.0, "Hz", 2.5),
        ParamInfo::continuous(F_RES, "Resonance", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(F_DRIVE, "Drive", 0.0, 24.0, 0.0, "dB"),
        ParamInfo::continuous(F_MORPH, "Morph", 0.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(FE_ON, "Filter Env", false),
        ParamInfo::continuous(FE_AMOUNT, "Filter Env Amount", -72.0, 72.0, 0.0, "st"),
        ParamInfo::continuous(FE_INITIAL, "Filter Env Initial", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(FE_PEAK, "Filter Env Peak", 0.0, 1.0, 1.0, ""),
        ParamInfo::continuous(FE_SUSTAIN, "Filter Env Sustain", 0.0, 1.0, 1.0, ""),
        ParamInfo::continuous(FE_END, "Filter Env End", 0.0, 1.0, 0.0, ""),
        ParamInfo::skewed(FE_ATTACK, "Filter Env Attack", 0.0, 20000.0, 0.1, "ms", 3.5),
        ParamInfo::skewed(FE_DECAY, "Filter Env Decay", 0.0, 20000.0, 500.0, "ms", 3.5),
        ParamInfo::skewed(
            FE_RELEASE,
            "Filter Env Release",
            0.0,
            20000.0,
            100.0,
            "ms",
            3.5,
        ),
        ParamInfo::continuous(FE_A_SLOPE, "Filter Env A Slope", -1.0, 1.0, 0.0, ""),
        ParamInfo::continuous(FE_D_SLOPE, "Filter Env D Slope", -1.0, 1.0, 0.0, ""),
        ParamInfo::continuous(FE_R_SLOPE, "Filter Env R Slope", -1.0, 1.0, 0.0, ""),
        ParamInfo::stepped(FE_LOOP, "Filter Env Loop", &ENV_LOOP, 0),
        ParamInfo::skewed(
            FE_LOOP_TIME,
            "Filter Env Loop Time",
            0.0,
            20000.0,
            0.0,
            "ms",
            3.5,
        ),
        ParamInfo::stepped(FE_REPEAT, "Filter Env Repeat", &SyncRate::LABELS, 12),
        ParamInfo::continuous(
            FE_TIME_VEL,
            "Filter Env Time < Vel",
            -100.0,
            100.0,
            0.0,
            "%",
        ),
        ParamInfo::toggle(SHAPER_ON, "Shaper", false),
        ParamInfo::stepped(
            SHAPER_TYPE,
            "Shaper Type",
            &["Soft", "Hard", "Sine", "4bit"],
            0,
        ),
        ParamInfo::continuous(SHAPER_AMOUNT, "Shaper Amount", 0.0, 1.0, 0.5, ""),
        ParamInfo::stepped(
            SHAPER_ORDER,
            "Shaper Order",
            &["Shaper > Filter", "Filter > Shaper"],
            0,
        ),
        ParamInfo::continuous(AE_INITIAL, "Aux Env Initial", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(AE_PEAK, "Aux Env Peak", 0.0, 1.0, 1.0, ""),
        ParamInfo::continuous(AE_SUSTAIN, "Aux Env Sustain", 0.0, 1.0, 1.0, ""),
        ParamInfo::continuous(AE_END, "Aux Env End", 0.0, 1.0, 0.0, ""),
        ParamInfo::skewed(AE_ATTACK, "Aux Env Attack", 0.0, 20000.0, 0.1, "ms", 3.5),
        ParamInfo::skewed(AE_DECAY, "Aux Env Decay", 0.0, 20000.0, 500.0, "ms", 3.5),
        ParamInfo::skewed(
            AE_RELEASE,
            "Aux Env Release",
            0.0,
            20000.0,
            100.0,
            "ms",
            3.5,
        ),
        ParamInfo::continuous(AE_A_SLOPE, "Aux Env A Slope", -1.0, 1.0, 0.0, ""),
        ParamInfo::continuous(AE_D_SLOPE, "Aux Env D Slope", -1.0, 1.0, 0.0, ""),
        ParamInfo::continuous(AE_R_SLOPE, "Aux Env R Slope", -1.0, 1.0, 0.0, ""),
        ParamInfo::stepped(AE_LOOP, "Aux Env Loop", &ENV_LOOP, 0),
        ParamInfo::skewed(
            AE_LOOP_TIME,
            "Aux Env Loop Time",
            0.0,
            20000.0,
            0.0,
            "ms",
            3.5,
        ),
        ParamInfo::stepped(AE_REPEAT, "Aux Env Repeat", &SyncRate::LABELS, 12),
        ParamInfo::continuous(AE_TIME_VEL, "Aux Env Time < Vel", -100.0, 100.0, 0.0, "%"),
        ParamInfo::stepped(AE_DEST_A, "Aux Dest A", &MOD_DESTS, 0),
        ParamInfo::stepped(AE_DEST_B, "Aux Dest B", &MOD_DESTS, 0),
        ParamInfo::continuous(AE_AMT_A, "Aux Amt A", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(AE_AMT_B, "Aux Amt B", -100.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(L1_ON, "LFO 1", false),
        ParamInfo::stepped(L1_TYPE, "LFO 1 Type", &LFO_TYPES, 0),
        ParamInfo::stepped(L1_SYNC, "LFO 1 Rate", &["Hz", "Beats"], 0),
        ParamInfo::skewed(L1_FREQ, "LFO 1 Freq", 0.01, 30.0, 1.0, "Hz", 3.0),
        ParamInfo::stepped(L1_BEATS, "LFO 1 Beats", &SyncRate::LABELS, 12),
        ParamInfo::skewed(L1_ATTACK, "LFO 1 Attack", 0.0, 20000.0, 0.0, "ms", 3.5),
        ParamInfo::toggle(L1_RETRIG, "LFO 1 Retrig", false),
        ParamInfo::continuous(L1_OFFSET, "LFO 1 Offset", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(L1_KEY, "LFO 1 Key", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(L1_TO_VOL, "LFO 1 Vol", 0.0, 100.0, 0.0, ""),
        ParamInfo::continuous(L1_TO_PAN, "LFO 1 Pan", 0.0, 100.0, 0.0, ""),
        ParamInfo::continuous(L1_TO_FILTER, "LFO 1 Filter", 0.0, 24.0, 0.0, ""),
        ParamInfo::continuous(L1_TO_PITCH, "LFO 1 Pitch", 0.0, 100.0, 0.0, ""),
        ParamInfo::toggle(L2_ON, "LFO 2", false),
        ParamInfo::stepped(L2_TYPE, "LFO 2 Type", &LFO_TYPES, 0),
        ParamInfo::stepped(L2_SYNC, "LFO 2 Rate", &["Hz", "Beats"], 0),
        ParamInfo::skewed(L2_FREQ, "LFO 2 Freq", 0.01, 30.0, 1.0, "Hz", 3.0),
        ParamInfo::stepped(L2_BEATS, "LFO 2 Beats", &SyncRate::LABELS, 12),
        ParamInfo::skewed(L2_ATTACK, "LFO 2 Attack", 0.0, 20000.0, 0.0, "ms", 3.5),
        ParamInfo::toggle(L2_RETRIG, "LFO 2 Retrig", false),
        ParamInfo::continuous(L2_OFFSET, "LFO 2 Offset", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(L2_KEY, "LFO 2 Key", 0.0, 1.0, 0.0, ""),
        ParamInfo::stepped(L2_STEREO, "LFO 2 Stereo", &["Phase", "Spin"], 0),
        ParamInfo::continuous(L2_PHASE, "LFO 2 Phase", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(L2_SPIN, "LFO 2 Spin", 0.0, 50.0, 0.0, "%"),
        ParamInfo::stepped(L2_DEST_A, "LFO 2 Dest A", &MOD_DESTS, 0),
        ParamInfo::stepped(L2_DEST_B, "LFO 2 Dest B", &MOD_DESTS, 0),
        ParamInfo::continuous(L2_AMT_A, "LFO 2 Amt A", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(L2_AMT_B, "LFO 2 Amt B", -100.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(L3_ON, "LFO 3", false),
        ParamInfo::stepped(L3_TYPE, "LFO 3 Type", &LFO_TYPES, 0),
        ParamInfo::stepped(L3_SYNC, "LFO 3 Rate", &["Hz", "Beats"], 0),
        ParamInfo::skewed(L3_FREQ, "LFO 3 Freq", 0.01, 30.0, 1.0, "Hz", 3.0),
        ParamInfo::stepped(L3_BEATS, "LFO 3 Beats", &SyncRate::LABELS, 12),
        ParamInfo::skewed(L3_ATTACK, "LFO 3 Attack", 0.0, 20000.0, 0.0, "ms", 3.5),
        ParamInfo::toggle(L3_RETRIG, "LFO 3 Retrig", false),
        ParamInfo::continuous(L3_OFFSET, "LFO 3 Offset", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(L3_KEY, "LFO 3 Key", 0.0, 1.0, 0.0, ""),
        ParamInfo::stepped(L3_STEREO, "LFO 3 Stereo", &["Phase", "Spin"], 0),
        ParamInfo::continuous(L3_PHASE, "LFO 3 Phase", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(L3_SPIN, "LFO 3 Spin", 0.0, 50.0, 0.0, "%"),
        ParamInfo::stepped(L3_DEST_A, "LFO 3 Dest A", &MOD_DESTS, 0),
        ParamInfo::stepped(L3_DEST_B, "LFO 3 Dest B", &MOD_DESTS, 0),
        ParamInfo::continuous(L3_AMT_A, "LFO 3 Amt A", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(L3_AMT_B, "LFO 3 Amt B", -100.0, 100.0, 0.0, "%"),
        ParamInfo::stepped(M_KEY_DEST_A, "Key Dest A", &MOD_DESTS, 0),
        ParamInfo::stepped(M_KEY_DEST_B, "Key Dest B", &MOD_DESTS, 0),
        ParamInfo::continuous(M_KEY_AMT_A, "Key Amt A", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(M_KEY_AMT_B, "Key Amt B", -100.0, 100.0, 0.0, "%"),
        ParamInfo::stepped(M_VEL_DEST_A, "Velocity Dest A", &MOD_DESTS, 0),
        ParamInfo::stepped(M_VEL_DEST_B, "Velocity Dest B", &MOD_DESTS, 0),
        ParamInfo::continuous(M_VEL_AMT_A, "Velocity Amt A", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(M_VEL_AMT_B, "Velocity Amt B", -100.0, 100.0, 0.0, "%"),
        ParamInfo::stepped(M_REL_DEST_A, "Release Vel Dest A", &MOD_DESTS, 0),
        ParamInfo::stepped(M_REL_DEST_B, "Release Vel Dest B", &MOD_DESTS, 0),
        ParamInfo::continuous(M_REL_AMT_A, "Release Vel Amt A", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(M_REL_AMT_B, "Release Vel Amt B", -100.0, 100.0, 0.0, "%"),
        ParamInfo::stepped(M_AT_DEST_A, "Aftertouch Dest A", &MOD_DESTS, 0),
        ParamInfo::stepped(M_AT_DEST_B, "Aftertouch Dest B", &MOD_DESTS, 0),
        ParamInfo::continuous(M_AT_AMT_A, "Aftertouch Amt A", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(M_AT_AMT_B, "Aftertouch Amt B", -100.0, 100.0, 0.0, "%"),
        ParamInfo::stepped(M_MW_DEST_A, "Mod Wheel Dest A", &MOD_DESTS, 0),
        ParamInfo::stepped(M_MW_DEST_B, "Mod Wheel Dest B", &MOD_DESTS, 0),
        ParamInfo::continuous(M_MW_AMT_A, "Mod Wheel Amt A", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(M_MW_AMT_B, "Mod Wheel Amt B", -100.0, 100.0, 0.0, "%"),
        ParamInfo::stepped(M_FC_DEST_A, "Foot Controller Dest A", &MOD_DESTS, 0),
        ParamInfo::stepped(M_FC_DEST_B, "Foot Controller Dest B", &MOD_DESTS, 0),
        ParamInfo::continuous(M_FC_AMT_A, "Foot Controller Amt A", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(M_FC_AMT_B, "Foot Controller Amt B", -100.0, 100.0, 0.0, "%"),
        ParamInfo::stepped(M_PB_DEST_A, "Pitch Bend Dest A", &MOD_DESTS, 0),
        ParamInfo::stepped(M_PB_DEST_B, "Pitch Bend Dest B", &MOD_DESTS, 0),
        ParamInfo::continuous(M_PB_AMT_A, "Pitch Bend Amt A", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(M_PB_AMT_B, "Pitch Bend Amt B", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(PB_RANGE, "Pitch Bend Range", 0.0, 24.0, 2.0, "st"),
    ]
}
