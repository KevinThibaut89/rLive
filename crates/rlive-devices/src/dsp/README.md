# DSP blocks

Shared building blocks for the built-in devices. Everything here is real-time safe: nothing on a
`process` path allocates, locks or does unbounded work, buffers are made at construction or in
`prepare(sample_rate, max_block)`, and the tests run every block under the allocation guard.
Stereo is left to the caller unless a block says otherwise: use one instance per channel.

The original small blocks (`Adsr`, `Svf`, `Biquad`, `DelayLine`, `Smoother`, `Oscillator`,
`soft_clip`) live in `basics.rs` and are re-exported from `crate::dsp`. Their sound is fixed by
golden tests, so devices depending on them don't change. New work should prefer the blocks below.

## Voices, modulation, envelopes, LFOs

### Voice allocator (`voice`)

Decides which voices play, and reports it through `VoiceEvent`s. The device owns the sound and
tells the allocator when a voice goes quiet. Handles polyphony, stealing, key retrigger, mono and
legato play, unison, glide, the sustain pedal and per-note expression by `NoteId`.

```rust
let mut alloc = VoiceAllocator::new(32);
alloc.prepare(sample_rate);
alloc.set(&VoiceSettings { polyphony: 8, unison: 2, ..Default::default() });

// In for_each_step, on BlockStep::Midi(message):
alloc.handle(message, |event| match event {
    VoiceEvent::Start { voice, note, stolen } => voices[voice].start(note, stolen),
    VoiceEvent::Legato { voice, note } => voices[voice].set_note(note),
    VoiceEvent::Release { voice } => voices[voice].release(),
    VoiceEvent::Kill { voice } => voices[voice].silence(),
});

// Rendering:
alloc.advance(frames);
let hz = midi_to_hz(alloc.pitch(v) + alloc.voice(v).note.spread * detune);
if voices[v].finished() { alloc.finished(v); }
```

### Modulation matrix (`modmatrix`)

Routes shared by all voices, with results per voice in a `ModBus`. A destination becomes
`(base + Σ source × amount) × Π (1 - amount + amount × source)`. Routes can be scaled by a second
source (`via`).

```rust
let mut matrix = ModMatrix::new(SOURCES, DESTS, 16);
matrix.set_route(0, Some(Route { source: LFO, dest: CUTOFF, amount: 24.0, kind: RouteKind::Add, via: None }));
let mut bus = matrix.bus(); // one per voice

matrix.evaluate(&voice_sources, &mut bus);
let cutoff = bus.apply(CUTOFF, base_cutoff);
```

### Envelopes (`envelope`)

Initial, peak, sustain and end levels, per-stage slopes, and Loop, Beat, Sync and Trigger modes.
Timing is sample accurate and doesn't drift. Retriggering and releasing start from the current
level, so neither clicks. Sync follows the song grid, and falls back to Beat while stopped.

```rust
let mut env = Envelope::new();
env.prepare(sample_rate);
env.set(&EnvelopeSettings { loop_mode: LoopMode::Sync, repeat_beats: 0.5, ..Default::default() });

env.set_clock(&BeatClock::from_transport(ctx.transport)); // each block, for Beat and Sync
env.gate_on();
let level = env.next();
```

### LFOs and tempo (`lfo`, `tempo`)

A stereo LFO with Phase (right channel ahead by a fraction of a cycle) and Spin (right channel runs
at a different rate). A `LfoRate::Beats` rate is worked out from the song position while playing,
so it can't drift; `tempo::SyncRate` has the usual note lengths for menus.

```rust
let mut lfo = Lfo::new(seed);
lfo.prepare(sample_rate);
lfo.set(&LfoSettings { rate: LfoRate::Beats(SyncRate::from_index(i).beats()), phase: 0.25, ..Default::default() });

lfo.set_clock(&BeatClock::from_transport(ctx.transport));
let [left, right] = lfo.next();
```

### Envelope follower (`follower`)

Peak or RMS level with separate attack and release, as one-pole time constants.

```rust
let mut follow = EnvelopeFollower::new();
follow.prepare(sample_rate);
follow.set_times(0.005, 0.1);
let level = follow.process(sample);
```

## Filters (`filter`)

Five zero-delay-feedback circuits (`Svf`, `Ladder`, `Ms2`, `Dfm`, `Prd`), each with low-pass,
band-pass, high-pass, notch and a morph between them, at 12 or 24 dB per octave, with drive. The
non-linear ones are solved from the previous state in a fixed amount of work per sample, stay
stable under any modulation, and self-oscillate near resonance 1. They're our own
circuit-inspired models and not copies of any existing filter. `FilterSettings::magnitude` gives
the small-signal curve for drawing.

```rust
let mut f = Filter::new();
f.prepare(sample_rate);
f.set(&FilterSettings { circuit: Circuit::Ladder, slope: FilterSlope::Db24, cutoff: 800.0, resonance: 0.6, drive: 6.0, ..Default::default() });

let y = f.process(x);
```

## Oversampling, sidechain, lookahead

### Oversampling (`oversample`)

Runs a closure at 2, 4 or 8 times the sample rate, with linear-phase half-band filters. Report
`latency()` from the device, and match the dry path to it.

```rust
let mut os = Oversampler::new(OversampleFactor::X4, max_block);
os.process_in_place(left, |hi| hi.iter_mut().for_each(|s| *s = s.tanh()));
```

### Sidechain detector (`sidechain`)

Builds a key from the main signal and an external one, with gain, mix, EQ and Listen, and follows
its level. Gain and mix ramp across a block, so moving them doesn't step.

```rust
let mut sc = SidechainDetector::new();
sc.prepare(sample_rate, max_block);
sc.set(&SidechainSettings { eq: SidechainEq::HighPass, eq_freq: 150.0, ..Default::default() });

let key = sc.process(ctx.audio.left, ctx.audio.right, ctx.sidechain);
for (x, level) in out.iter_mut().zip(key.level) { /* ...reduce gain from *level... */ }
sc.listen(&mut ctx.audio);
```

### Lookahead (`lookahead`)

An exact delay of `N`-channel frames, reporting the loudest sample in the window about to leave.

```rust
let mut look = LookaheadBuffer::<2>::new(max_samples);
look.set_len(samples);

let [l, r] = look.process([left, right]);
let peak = look.window_peak();
```

## Spectral

### STFT (`stft`)

Streams audio through a spectral process with perfect reconstruction, at `size` samples of
latency. The hop must divide the size and be at most half of it.

```rust
let mut stft = Stft::new(2048, 512);
stft.process(input, output, |bins| bins[0..8].fill(Default::default()));
```

### Pitch detection (`pitch`)

YIN, with the difference function computed by an FFT, so an estimate costs two transforms. About
1-2 cents below 2 kHz.

```rust
let mut detector = PitchDetector::new(sample_rate, 40.0, 4_000.0, 512);
if let Some(pitch) = detector.process(block) { /* pitch.hz, pitch.confidence */ }
```

### Spectrum feed (`spectrum`)

Measures a stream and publishes `SCOPE_VALUES` log-spaced bands in `[0, 1]`, at nothing but a check
while no UI is reading.

```rust
let mut feed = SpectrumFeed::new(sample_rate, 4096, 1024);
feed.process_stereo(ctx.audio.left, ctx.audio.right, ctx.view, ViewId(0));
```

## Convolution, generated responses and tables

### Convolution (`convolution`)

Uniformly partitioned, with a constant cost per partition and a latency of one partition. A
`ConvolverIr` is made off the audio thread and shared; swapping one in doesn't allocate and follows
the `set_resource` contract.

```rust
let ir = Arc::new(ConvolverIr::new(&impulse, 256));
let mut conv = Convolver::with_ir(ir);
conv.process(input, output);
```

### Impulse responses (`ir`)

Original responses made from code (`Room`, `Hall`, `Plate`, `Spring`, `Cabinet`). Same spec and
seed, same response. No sampled content.

```rust
let response = generate_ir(&IrSpec { kind: IrKind::Hall, decay: 2.0, ..Default::default() }, sample_rate);
```

### Wavetables (`wavetable`)

A set of single-cycle frames stored at several bandwidths, read with interpolation between frames.
`generate_wavetable` makes original tables from code; `Wavetable::from_frames` takes your own.

```rust
let table = generate_wavetable(TableKind::BasicShapes, 64, seed);
let mut osc = WavetableOsc::default();
let sample = osc.next(&table, position, hz / sample_rate);
```

## Granular and delay

### Granular (`granular`)

Overlapping Hann-windowed grains from a fixed pool, reading a live `GrainRing` or a `GrainSample`.
Grains stay inside what's readable however they're pitched, can play in reverse, and can't click.

```rust
let mut grains = GranularEngine::new(64, seed);
grains.prepare(sample_rate);
grains.set(&GrainSettings { rate: 40.0, size: 0.08, position: 0.2, ..Default::default() });

ring.write(input);
let [left, right] = grains.next(&ring);
```

### Delay lines (`delay`)

`InterpDelay` reads fractional delays with linear or Catmull-Rom interpolation. `SmearDelay` spreads
a delay's output in time over a bell of taps, keeping the level of a steady signal.

```rust
let mut delay = InterpDelay::new(max_samples);
delay.write(x);
let y = delay.read(time_in_samples, Interpolation::Cubic);
```

## Utilities (`util`)

`sanitize` (non-finite and denormal to zero, for feedback paths), `db_to_gain`, `midi_to_hz`,
`lerp`, `cubic` and a seeded `Rng`.
