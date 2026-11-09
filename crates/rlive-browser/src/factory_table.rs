const TABLE: &[Row] = &[
    // Dynamics
    ("rlive.compressor", "Gentle Glue", &["Warm", "Soft"], &[("Threshold", -18.0), ("Ratio", 2.0), ("Attack", 30.0), ("Release", 120.0), ("Knee", 6.0), ("Makeup", 2.0)]),
    ("rlive.compressor", "Tight Drums", &["Hard"], &[("Threshold", -24.0), ("Ratio", 6.0), ("Attack", 3.0), ("Release", 60.0), ("Makeup", 6.0)]),
    ("rlive.compressor", "New York Blend", &["Hard"], &[("Threshold", -32.0), ("Ratio", 10.0), ("Attack", 1.0), ("Release", 80.0), ("Makeup", 10.0), ("Dry/Wet", 0.4)]),
    ("rlive.gate", "Clean Up Noise", &["Clean"], &[("Threshold", -50.0), ("Return", 6.0), ("Attack", 0.5), ("Hold", 30.0), ("Release", 120.0)]),
    ("rlive.gate", "Choppy Gate", &["Hard"], &[("Threshold", -24.0), ("Return", 2.0), ("Attack", 0.05), ("Hold", 5.0), ("Release", 15.0), ("Floor", -70.0)]),
    ("rlive.limiter", "Brickwall Safety", &["Clean"], &[("Gain", 0.0), ("Ceiling", -0.3), ("Release", 300.0), ("Auto Release", 1.0)]),
    // EQ and filters
    ("rlive.eq3", "Kill the Lows", &["Bright"], &[("Low", -70.0), ("Low Freq", 250.0)]),
    ("rlive.eq3", "Smile", &["Bright"], &[("Low", 3.0), ("Mid", -3.0), ("High", 3.0)]),
    ("rlive.auto_filter", "Dark Lowpass", &["Dark"], &[("Freq", 600.0), ("Res", 25.0)]),
    ("rlive.auto_filter", "Resonant Sweep Start", &["Evolving"], &[("Freq", 1800.0), ("Res", 70.0), ("Drive", 20.0)]),
    ("rlive.utility", "Mono Below", &["Clean"], &[("Bass Mono", 1.0), ("Bass Mono Freq", 150.0)]),
    ("rlive.utility", "Super Wide", &["Wide"], &[("Width", 1.8)]),
    // Distortion
    ("rlive.saturator", "Tape Warmth", &["Warm"], &[("Drive", 6.0), ("Output", -3.0), ("Dry/Wet", 0.8)]),
    ("rlive.saturator", "Hot Signal", &["Dirty"], &[("Drive", 18.0), ("Output", -9.0)]),
    ("rlive.overdrive", "Small Amp Crunch", &["Dirty"], &[("Drive", 0.6), ("Tone", 0.55), ("Dry/Wet", 0.9)]),
    ("rlive.redux", "Eight Bit Friend", &["Dirty", "Bright"], &[("Rate", 11025.0), ("Bits", 8.0), ("Dry/Wet", 1.0)]),
    // Delay and modulation
    ("rlive.delay", "Dotted Slapback", &["Bright"], &[("Sync", 1.0), ("Feedback", 0.25), ("Dry/Wet", 0.25)]),
    ("rlive.delay", "Wide Ping Pong", &["Wide"], &[("Sync", 1.0), ("Ping Pong", 1.0), ("Feedback", 0.45), ("Dry/Wet", 0.3)]),
    ("rlive.delay", "Dub Tail", &["Dark", "Evolving"], &[("Sync", 1.0), ("Feedback", 0.75), ("Tone", 1800.0), ("Dry/Wet", 0.4)]),
    ("rlive.chorus_ensemble", "Soft Shimmer", &["Soft", "Wide"], &[("Rate", 0.6), ("Amount", 40.0)]),
    ("rlive.phaser_flanger", "Slow Sweep", &["Evolving"], &[("Notches", 6.0), ("LFO Freq", 0.2), ("Blend", 60.0)]),
    // Reverb
    ("rlive.reverb", "Small Wood Room", &["Warm"], &[("Size", 0.25), ("Damping", 0.6), ("Dry/Wet", 0.2)]),
    ("rlive.reverb", "Long Plate Wash", &["Bright", "Wide"], &[("Size", 0.85), ("Damping", 0.2), ("Dry/Wet", 0.35), ("Pre-delay", 25.0)]),
    ("rlive.reverb", "Cathedral of Dust", &["Dark", "Evolving"], &[("Size", 0.98), ("Damping", 0.75), ("Dry/Wet", 0.5), ("Pre-delay", 60.0)]),
    // MIDI
    ("rlive.chord", "Minor Stack", &[], &[("Shift 1", 3.0), ("Shift 2", 7.0)]),
    ("rlive.chord", "Open Fifths", &[], &[("Shift 1", 7.0), ("Shift 2", 12.0)]),
    ("rlive.arpeggiator", "Steady Up", &[], &[("Style", 0.0), ("Sync", 1.0), ("Gate", 60.0)]),
    ("rlive.velocity", "Even Hands", &["Soft"], &[("Compand", 40.0)]),
    ("rlive.random", "Gentle Wander", &["Evolving"], &[("Chance", 25.0), ("Choices", 5.0), ("Interval", 2.0)]),
    // Instruments
    ("rlive.poly", "Round Sub", &["Bass", "Warm"], &[("Osc Mix", 0.0), ("Cutoff", 400.0), ("Resonance", 0.1), ("Attack", 2.0), ("Decay", 300.0), ("Sustain", 0.7)]),
    ("rlive.poly", "Warm Pad", &["Pad", "Warm", "Evolving"], &[("Osc Mix", 0.5), ("Osc 2 Fine", 8.0), ("Cutoff", 1400.0), ("Attack", 800.0), ("Decay", 2000.0), ("Sustain", 0.8)]),
    ("rlive.poly", "Bright Stab", &["Keys", "Bright", "Hard"], &[("Cutoff", 6000.0), ("Env Amount", 3.0), ("Attack", 1.0), ("Decay", 220.0), ("Sustain", 0.1)]),
    ("rlive.juno106", "Velvet Strings", &["Strings", "Soft"], &[("Saw", 1.0), ("VCF Freq", 6.0), ("VCF Res", 1.0)]),
    ("rlive.juno106", "Hollow Square Bass", &["Bass", "Dark"], &[("Pulse", 1.0), ("Sub", 6.0), ("VCF Freq", 3.0)]),
    ("rlive.simpler", "Sampler Keys Start", &["Keys"], &[("Gain", -3.0)]),
];
