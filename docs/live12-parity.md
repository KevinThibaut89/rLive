# Live 12 parity checklist

The definition of done for the rLive parity roadmap. It is written in our own words, with page numbers from the Ableton Live 12 reference manual. Read those pages for exact behaviour when working on a milestone.

**Status:** `[ ]` not started · `[x]` built with an automated check · `[~]` built, needs a human or hardware check · `[out]` out of scope.

Features that rLive already had when this list was created stay `[ ]` until their milestone audits them against the manual.

**Tags:** effort is S, M, L or XL, as rough estimates. Other tags mark a dependency: PROP, [P] or {PM} for proprietary or modelled algorithms; ML, [ML] or {ML?} for machine learning; CLOUD, [C] or {SVC} for Ableton services; CONTENT, [O], {CONTENT} or {SAMPLES} for Ableton-owned content; HW, [H] or {HW} for specific hardware; MAX, [X] or {MAX} for Max.


## 1. Welcome (p. 23)

**Milestones:** None

- `[ ]` No features.


## 2. First Steps (pp. 24-34)

**Milestones:** C1, A16

- `[out]` Licence authorization, two machines per licence [M CLOUD]
- `[out]` Learn View: video lessons, topic filter, completion tracking, resume last page, reload; online only [L CLOUD CONTENT]
- `[out]` Always-on-top picture-in-picture lesson video, 15 s skips, resumes position [M CLOUD]
- `[~]` Info View: name and purpose of any hovered control [M] → control metadata registry
- `[~]` User info text on tracks, clips, devices, scenes, locators, saved in Set [S] → Info View
- `[x]` Settings window with 10 tabs [S]
- `[ ]` Display: UI language [L]; independent zoom for main, second and Settings windows [M]
- `[ ]` Focus outline, scroll bar mode, Follow behaviour, show/hide labels [S]
- `[ ]` Keyboard: Tab moves focus, continuous Tab within section, arrows move clips [M]
- `[ ]` Input: Pen Tablet Mode, Permanent Scrub Areas (default on), pitch-locked drawing [S each]
- `[ ]` Restore suppressed warning dialogs [S]
- `[x]` Themes: named themes, light/dark/follow OS, warm/cool/neutral tone, high contrast [M]
- `[x]` Grid opacity, brightness, colour intensity, hue [S]
- `[x]` Auto track colours, default colour, reduced palette; new clips random or track colour [S]
- `[ ]` Audio: driver type, CoreAudio/ASIO, device I/O, sample rate, latency, calibration tests [M]; macOS follow-system output [S]
- `[ ]` Link and Link Audio configuration [L PROP] → ch. 37
- `[ ]` Tempo & MIDI: per-port roles for notes, sync, remote; resync hardware to clock [M] → chs. 34, 37
- `[ ]` File & Folder: analysis files toggle, external sample editor, temp folder, cache options [S each]; Max application choice [S MAX]
- `[ ]` Library: install and User Library locations, collect-on-export policy, show/hide Cloud/Push/Splice [S]
- `[ ]` Plug-Ins: folder locations, enabled folders, window behaviour [M]
- `[ ]` Record, Warp & Launch: new-recording file type, warp and fade defaults, launch, selection and recording behaviour [M]
- `[out]` Licences & Updates: authorization, auto-update, usage data [M CLOUD]


## 3. Live Concepts (pp. 35-61)

**Milestones:** Overview; see the chapters that define each feature

Overview chapter; only behaviours stated here or foundational are listed.
- `[ ]` Control Bar with nine sections: browser, tempo/metronome, scale, follow/position, transport, automation/capture, loop/punch, MIDI/CPU, view selector [M]
- `[ ]` Status Bar: errors, update notices, note details, insert marker position, clickable warnings [S]
- `[ ]` Similarity Search: ranked similar items for samples up to 60 s and instrument/drum presets, across Core and User Library [L ML; Core part CONTENT]
- `[ ]` Background sound analysis with status and pause; Core content pre-analysed [M ML CONTENT]
- `[ ]` Similar Sample Swapping in Drum Rack, Simpler, Drum Sampler [M ML] → Similarity Search, those devices
- `[ ]` Clips exist in two independent collections, Session and Arrangement, over one shared track set [XL]
- `[ ]` Track plays one clip at a time; Session clip overrides Arrangement until Back to Arrangement, global and per track [L]
- `[ ]` View switching affects UI only, never playback [S]
- `[ ]` Second window; Tab swaps views between windows [L]
- `[ ]` Audio and MIDI clips restricted to matching track type [S]
- `[ ]` Audio clip is a sample reference plus settings, all processing real-time [L]
- `[ ]` MIDI files absorbed into the Set, no reference kept [S]
- `[ ]` Device chain order: MIDI effects, instrument, audio effects; audio/group/return tracks audio only [L]
- `[ ]` Device connections always stereo; hardware I/O configurable mono [S]
- `[ ]` MIDI track with no instrument or audio effects outputs MIDI; mix and send controls vanish [S]
- `[ ]` Stacked Clip and Device View [S]
- `[ ]` Scale Mode per clip with root and scale; Control Bar mirrors selection and sets new-clip defaults [M]
- `[ ]` Scale awareness across MIDI Tools, utilities, Arpeggiator, Chord, Pitch, Random, Scale, Auto Shift, Meld [M] → those devices
- `[ ]` Mixer sections shown or hidden individually [S]
- `[ ]` Sends and returns; returns hold effects only [M]
- `[ ]` Crossfader across any number of tracks including returns [M]
- `[ ]` Presets stored outside Sets; Racks save multi-device combinations [L]
- `[ ]` In/Out patchbay: external, track-to-track, device routing, resampling, group submix [L]
- `[ ]` Monitoring modes for inputs [M]
- `[ ]` External Audio Effect and External Instrument [M HW-optional]
- `[ ]` Arm, multi-arm, Exclusive Arm, auto-arm on instrument insert [S]
- `[ ]` Arrangement Record: one new clip per armed track per take [M]
- `[ ]` Session Record into selected scene; second press stops and launches; quantized [M] → launch quantization
- `[ ]` Overdub and record quantization for drum patterns [M]
- `[ ]` Automation Arm, record to Arrangement or Session clips; manual change overrides until Re-Enable [L]
- `[ ]` Clip envelopes for device, mixer, pitch, volume, MIDI CC; unlinked loops [L]
- `[ ]` MIDI Map and Key Map modes [L]; mapped messages filtered from track input [S]
- `[ ]` Session clips mapped to key range for chromatic play [M]
- `[ ]` Dedicated Push 1/2/3 support [XL HW]


## 4. Working with the Browser (pp. 62-121)

**Milestones:** C12, A19

**Layout and search**
- `[ ]` Sidebar, content pane, filters, results bar, preview tab, resizable, full-height option [M]
- `[ ]` Content columns: show, reorder, sort, file-extension toggle [S]
- `[ ]` AND-matched search across labels; shortcut jumps to All [M] → indexed database
- `[ ]` Hashtag tag search with autocomplete [S]
- `[ ]` Keyboard-only search flow; Esc clears terms and tags [S]
- `[ ]` Clear button resets terms and tags; x clears terms only [S]
- `[ ]` Save results as custom label; rename labels, change icons, delete [S]
- `[ ]` Browser back/forward history of searches and navigation [S]

**Tags**
- `[ ]` Filter groups per label, state remembered per label, multi-select, hide, reset [M]
- `[ ]` Factory tags immutable; user tags additive [S CONTENT]
- `[ ]` Auto tags from sound analysis for samples up to 60 s, toggleable, editable [M ML]
- `[ ]` VST3 subcategory mapped to tags [S]
- `[ ]` Tag Editor: check/uncheck, batch tagging, custom tags and groups, nested subtags, rename, delete [M]
- `[ ]` Quick Tags: inline assign, autocomplete, create new, parent prompt, asterisk for partial [M]

**Collections and Library**
- `[ ]` Seven colour labels, number-key assignment, 0 clears, multiple colours per item (three shown), rename, show/hide [S]
- `[ ]` Library labels: All, Sounds, Drums, Instruments, Audio Effects, MIDI Effects, Modulators, Max for Live, Plug-Ins, Clips, Samples, Grooves, Templates, Tunings; show/hide [M; Max label MAX]

**Places**
- `[out]` Packs: installed, updates, available; pause/resume/cancel, batch download, install, size, delete [L CLOUD CONTENT]
- `[ ]` Pack Info pages and Help overview [S CONTENT]
- `[out]` Splice: embedded catalogue, device-code login, search suggestions [XL CLOUD]
- `[out]` Splice filters (instrument, genre, key, tempo, type), sort, shuffle, category browse [in above, CLOUD]
- `[out]` Splice Search with Sound from clip or time selection, 50 results [L CLOUD ML]
- `[out]` Splice preview: loop, tempo sync, transpose, scale sync, loudness; Likes, Collections, share [in above, CLOUD]
- `[out]` Splice drag to track or device, auto-licensed, Complex warp forced, download location options [M CLOUD] → Complex warp
- `[out]` Ableton Cloud: browser sign-in, up to eight synced Sets from Note and Move, one-way into Live [L CLOUD HW]
- `[ ]` Push 3 Standalone: Wi-Fi discovery, six-digit pairing, browse and transfer Projects, unpair [L HW]
- `[out]` User Library: portable, relocatable, default folders (Clips, Defaults, Grooves, Presets, Samples, Templates, Chord Banks, ABL Assets) [M; ABL Assets CLOUD]
- `[ ]` Current Project label: expand Set into tracks, clips, devices, returns, Main, grooves [M] → Set parser
- `[ ]` Backup folder: last ten saves, timestamps, oldest trashed [S]
- `[ ]` User Folders: add, drag-add, scan, spinner, greyed when missing, Locate, Remove [M]

**Navigation, preview, insertion**
- `[ ]` Arrow navigation, modifier keeps folders open, drag-scroll [S]
- `[ ]` Preview on selection or by shortcut [M]
- `[ ]` Non-Raw preview starts on next bar, tempo-matched, looped; Raw plays original tempo, unlooped [M] → warping
- `[ ]` Preview scrub (not Raw, not unwarped saved clips), preview/cue volume, separate cue outs [M]
- `[ ]` Drag to track, or to empty area to create track [M]
- `[ ]` Double-click/Enter loads to selected track; samples go to Simpler on MIDI tracks, slot on audio tracks [S] → Simpler
- `[ ]` Drag from OS file manager [S]


## 5. Managing Files and Sets (pp. 122-164)

**Milestones:** A6, C13, C14

**Samples**
- `[ ]` Native WAV, AIFF, AIFF-C, FLAC, Ogg Vorbis; import MP3, M4A (Mac) [M]
- `[ ]` Disk streaming of any length, rate or depth, mono or stereo [L]
- `[ ]` Decoding cache: minimum free space, maximum size, cleanup, location, auto-eviction [M]
- `[ ]` Analysis sidecar: warp data, tempo, waveform, default clip; icon indicator; disable option [M]; .asd format itself [PROP if interop wanted]
- `[ ]` Save Default Clip into sidecar, applies to future imports only [S] → sidecar
- `[ ]` Waveform greyed and unplayable during analysis [S]

**Export**
- `[ ]` Rendered Track: Main, All Individual, Selected, single named track; post-fader, equal lengths [M] → offline render
- `[ ]` MIDI tracks with no output skipped; audio effects with no instrument exports silence [S]
- `[ ]` Start and length, default whole Set or time selection [S]
- `[ ]` Render reflects currently active Session or Arrangement clips [M]
- `[ ]` Include Return and Main Effects [M]
- `[ ]` Render as Loop: two passes, tail wrapped to start [M]
- `[ ]` Mono, Normalize, Create Analysis File [S each]
- `[ ]` Sample rate selection; downsampling via SoX in second step [S]
- `[ ]` PCM WAV/AIFF/FLAC, 16/24/32-bit [S]
- `[ ]` Dither: none, rectangular, triangular (default) [S]; POW-r 1-3 [M PROP]
- `[ ]` MP3 CBR 320, PCM optional [S]; patent-free now, needs encoder library
- `[ ]` Video render with system encoder, settings dialog, needs PCM and Arrangement video [L]
- `[ ]` Real-time render for external devices: auto signal trace, 10 s editable wait, skip, restart, auto-restart on dropout [M] → External devices

**MIDI and Live Clips**
- `[ ]` Import MIDI at insert marker or slot; Standard MIDI export of a clip [S]
- `[ ]` Live Clip format: clip, envelopes and track devices; devices restored only on empty track [M]
- `[ ]` Drag clip to browser saves, with copy policy [M]

**Sets**
- `[ ]` New, Open, Open Recent, Save, Save As, Save a Copy [M]
- `[ ]` Undo History window: jump to any step, greyed redo region, not persisted [M] → command-based undo [L]
- `[ ]` Merge Sets: drag whole Set, excludes returns, rebuilds clips, devices, automation [L]
- `[ ]` Unfold Sets in browser: tracks, groups, devices, Session clips, grooves individually importable [M]
- `[ ]` Export selected Session clips as new Set by drag [M]
- `[ ]` Default Set and named templates, load as Untitled [S]
- `[ ]` File reference list: replace, hot-swap, external edit (goes offline), location, usage expansion [M]
- `[ ]` Warp markers kept on replace only if length suitable [S]

**Projects**
- `[ ]` Project folder auto-created on save; shared samples across versions; title bar display [M]
- `[ ]` Saving outside Project creates new one, external references remain [S]
- `[ ]` Older-version Sets require Save As [S]
- `[ ]` Presets default into current Project [S]
- `[ ]` Offline clip marking, silence, Status Bar warning [S]
- `[ ]` Manual repair by drag; no identity check [S]
- `[ ]` Automatic search: folder, Project, Library, full rescan; 0/1/many candidates, hot-swap for many [M]
- `[ ]` Collect All and Save by location; File Manager per-location counts, sizes, Show [M]
- `[ ]` Collect Files on Export: Always (default), Ask, Never [S]
- `[ ]` File Manager scopes: Set, Project, User Library, any Project via context menu [M]
- `[ ]` Unused files by category, preview, delete [M]
- `[ ]` Pack creation: .alp lossless archive, install by double-click, drag, menu; own archive format is enough unless interop wanted [M PROP-if-interop]


## 6. Arrangement View (pp. 165-186)

**Milestones:** A7, C4, C5, C6

- `[ ]` Overview strip: drag scroll/zoom, double-click full [M]
- `[ ]` Beat-time and min-sec-ms rulers; drag zoom; double-click zoom to selection [M]
- `[ ]` Zoom: +/-, modifier scroll, zoom to selection with step-back history [S]
- `[ ]` Track height: unfold, drag, pinch, all-tracks modifier, unfold all, Optimize Height/Width [M]
- `[ ]` Waveform vertical zoom applied to all audio tracks [S]
- `[ ]` Follow with auto-pause on edit or scroll, resume on restart or scrub click [S]
- `[ ]` Play, Stop, space, continue from stop point, double-stop to start [S]
- `[ ]` Insert marker as play origin, keyboard-movable across tracks and to edges [M]
- `[ ]` Arrangement Position fields: drag, type, arrows [S]
- `[ ]` Scrub click starts quantized; hold repeats quantized region [M] → global quantize
- `[ ]` Chase MIDI Notes toggle [M]
- `[ ]` Locators: add in real time (quantized), context menu, delete, rename, info text, MIDI/key map, double-click plays [M]
- `[ ]` Previous/Next locator with quantized jump [S]
- `[ ]` Loop to Next Locator; Set Song Start Time Here on locator or loop brace [S]
- `[ ]` Time signature markers: numerators 1-2 digits, denominators 1/2/4/8/16, unquantized, marker lane hidden if none [L] → tempo map
- `[ ]` Fragmentary bars with hatch display; Delete Fragmentary Bar Time, Complete Fragmentary Bar [M]
- `[ ]` Import MIDI time signatures option [S]
- `[ ]` Arrangement loop: numeric fields, Loop Selection, brace drag, arrow nudges, double/halve [M]
- `[ ]` Punch in/out [M]
- `[ ]` Clip move, cross-track move, edge resize, snap to grid, clips, locators, signatures [L]
- `[ ]` Slide clip content within bounds; snap bypass [M]
- `[ ]` Fades: handles, curve, limits (no crossing loop bounds, no overlap), limit indicator, F-key momentary [L]
- `[ ]` Crossfades by drag or selection command [M]
- `[ ]` Create Fades on Clip Edges: 4 ms default fades and crossfades; Delete resets [S]
- `[ ]` Selection model: clip, insert point, timespan, in-clip time, Shift-extend, loop-brace select [L]
- `[ ]` Deactivate clips with 0; also tracks when header selected [S]
- `[ ]` Reverse multi-clip audio selection [M] → Reverse
- `[ ]` Nudge selection with arrows [S]
- `[ ]` Grid: adaptive or fixed, narrow, widen, triplets, snap toggle, modifier bypass, spacing display [M]
- `[ ]` Cut/Paste/Duplicate/Delete Time, Insert Silence, moves signatures too [M]
- `[ ]` Split at point or selection [M]
- `[ ]` Consolidate across tracks: renders pre-effects, includes warp, attenuation, clip envelopes; stored Processed/Consolidate [L] → offline clip renderer
- `[ ]` Linked tracks: multiple exclusive groups, hover highlight, click selects, add/unlink, group shortcut [L]
- `[ ]` Linked operations: move, resize, select, time commands, split, consolidate, fades, arm, take lanes [L] → comping (ch. 21)
- `[ ]` Mixer in Arrangement plus Arrangement track controls, shared values, customizable sets; some mixer-only [M]


## 7. Session View (pp. 187-199)

**Milestones:** A11, C7, C17

- `[ ]` Clip Launch and Stop per slot; keyboard select and Enter launch, arrows move [L]
- `[ ]` Transport keeps running after clips stop; double Stop resets [S]
- `[ ]` Multi-select rename with Tab flow, info text, colours [S]
- `[~]` Group slots: shading by left-most clip colour, group launch and stop, click selects children [M]
- `[ ]` Track resize to launch-only width, all-tracks modifier [S]
- `[ ]` Scene launch; cancel pending launch [M]
- `[ ]` Select Next Scene on Launch [S]
- `[ ]` Scene rename, colour, info text, reorder (non-adjacent collapse by mouse, retained by keyboard) [M]
- `[ ]` Position-derived scene numbers [S]
- `[ ]` Scene Tempo 20-999 and Time Signature, hidden by default, disable by reset [M] → tempo engine
- `[ ]` Coloured launch button for scenes with tempo/meter [S]
- `[ ]` Tab/arrow navigation across scene fields [S]
- `[ ]` Legacy name-encoded tempo migrated [S; only if importing old Sets]
- `[ ]` Scene View panel: tempo, signature, Follow Actions, multi-scene edit [M] → ch. 16
- `[ ]` Track Status: loop pie with counts, one-shot remaining time, input monitor icon, Arrangement mini-display; click opens clip [M]
- `[ ]` Multi-file drop: one track by default, modifier spreads across tracks [S]
- `[ ]` Rubber-band and modifier selection, drag moves [M]
- `[ ]` Select on Launch toggle [S]
- `[ ]` Add/Remove Stop Button, stopless scene behaviour [S]
- `[ ]` Insert Scene [S]
- `[ ]` Capture and Insert Scene: copy running clips, launch seamlessly [L]
- `[ ]` Arrangement Record logs launches, clip property edits, automation, tempo/meter; no new audio [L]
- `[ ]` Stop All Clips [S]
- `[ ]` Drag or paste clips between views; Arrangement to Session lays out top to bottom [M]
- `[ ]` Consolidate Time to New Scene [M] → Consolidate


## 8. Clip View (pp. 200-233)

**Milestones:** C2

- `[ ]` Panels plus editor, resizable, auto-close at bottom, fold, opens in focused window, F12 moves between windows [M]
- `[ ]` Panels horizontal, vertical, or automatic by height [M]
- `[ ]` Clip Activator, multi-clip [S]
- `[ ]` Name independent of file, colour, assign track colour to clips, group colour to children and clips, view-scoped [S]
- `[ ]` Editor modes: Sample, Envelope, Notes, MPE, shortcut cycle [M]
- `[ ]` Start/End and Set buttons quantized during playback; beats if warped, time if not [M]
- `[ ]` Loop toggle (needs Warp for audio); Position, Length, on-the-fly Set buttons [M]
- `[ ]` Loop controls MIDI mappable [S] → MIDI Map
- `[ ]` Clip time signature, display only, polymeter aid [S]
- `[ ]` Clip Groove chooser, hot-swap, Commit [S] → ch. 14
- `[ ]` Commit positive-velocity groove writes volume envelope, overwriting [S]
- `[ ]` Clip scale on audio forwards to scale-aware devices only [S]
- `[ ]` Extended properties, launch controls only for Session clips [S]
- `[ ]` Bank/sub-bank/program per MIDI clip, 128 each, sent on launch [S]
- `[ ]` Reverse creates new file, flips markers and regions, envelopes stay, cached for instant re-reverse, progress bar [M]
- `[ ]` External destructive edit, needs playback stopped, markers kept if length same, affects other clips [M]
- `[ ]` Session clip edge fade 0-4 ms, signal-dependent [S]
- `[ ]` RAM Mode [S]
- `[ ]` Hi-Q interpolation, about 19 semitones before aliasing [M]; method not disclosed, generic alternatives exist
- `[ ]` Gain in dB, pitch semitones plus cents, range display on multi-select [S]
- `[ ]` Pitch utilities: Transpose, Fit to Scale, Invert, Interval [S each] → ch. 10
- `[ ]` Time utilities: Stretch, x2, /2, Duration, Humanize, Reverse, Legato [S each]
- `[ ]` Audio Transform panel: Quantize, grid or value, Amount [S] → warp
- `[ ]` Zoom, scroll, Follow, selector overview as in Arrangement [M]
- `[ ]` Markers draggable or arrow keys, modifier moves region [S]
- `[ ]` Clip scrub jumps by global quantize; hold repeats [M]
- `[ ]` Global quantize shortcuts [S]
- `[ ]` Loop brace keys, Duplicate Loop shifts following notes, run-in before loop [M]
- `[ ]` Sample details header; asterisks when mixed; click reveals in browser [S]
- `[ ]` Crop Clip Sample to region or time selection creates new file; MIDI crop has no file [M]
- `[ ]` Replace sample by drop or Enter; markers kept only for identical length [S]
- `[ ]` Show Similar Files [S ML] → Similarity
- `[ ]` Manage Sample File opens File Manager [S]
- `[ ]` Multi-clip properties: shared only, range sliders collapse at extremes [M]
- `[ ]` Clip Update Rate quantizes applied edits [M]; default Launch and Warp Mode for new clips [S]


## 9. Audio Clips, Tempo, and Warping (pp. 234-251)

**Milestones:** A7, A9, A10

- `[ ]` Tempo field coarse and fine, separately mappable, automatable [M]
- `[ ]` Tap Tempo, mappable; Start Playback with Tap Tempo, four taps in 4/4, Link-aligned [S]
- `[ ]` Phase Nudge Up/Down [S]
- `[ ]` Tempo Leader clips (Arrangement): Set follows markers, field disabled, bottom track wins [L]
- `[ ]` Leader-generated non-editable automation; Unfollow Tempo Automation; overrides Tempo Follower; disabled under EXT [M]
- `[ ]` Warp toggle; beat-synced follow [XL] → stretch engine, tempo map
- `[ ]` Short sample default: Unwarped One Shot, Warped One Shot, Warped Loop, Auto (default) [M]
- `[ ]` Auto-Warp Long Samples default on; marker on each bar [XL PROP] → tempo/beat tracking
- `[ ]` Default Warp Mode, Beats initially [S]
- `[ ]` Import Audio File command [S]
- `[ ]` Warp Markers: add, move, arrow nudge, delete, Shift moves waveform under marker [L]
- `[ ]` Transient detection with visible markers; manual add, delete, reset [L]
- `[ ]` Pseudo-markers on hover, modifier creates adjacent ones, Shift moves transient [M]
- `[ ]` Insert Warp Markers from selection transients [S]
- `[ ]` Save markers with sample; not for Core/Pack content [S CONTENT]
- `[ ]` Even-length loop assumption 1-16 bars, two markers [M]
- `[ ]` BPM field, fine drag, x2/÷2, Shift-drag clip edge stretches tempo [S]
- `[ ]` Set 1.1.1 Here [S]
- `[ ]` Multi-clip warping across same-length clips [M]
- `[ ]` Warp Sample As, Warp Selection As with suggested length [M] → Auto-Warp
- `[ ]` Warp From Here, Start At, Straight, BPM From Here [M] → Auto-Warp
- `[ ]` Audio quantize: nearest transient to grid, Amount, triplets [M] → transients, markers
- `[ ]` Beats mode: Preserve transients or grid; Loop Off/Forward/Back-and-Forth with zero-crossing; Transient Envelope 0-100 [L PROP]
- `[ ]` Tones mode: pitch-adaptive grain size [L PROP]
- `[ ]` Texture mode: grain size, Fluctuation [M PROP]
- `[ ]` Re-Pitch: rate change, transpose disabled [S]
- `[ ]` Complex [XL PROP]; Complex Pro: Formants 0-100%, Envelope default 128 [XL PROP]. Exact sound is not reproducible; substitutes are a licensed or open-source stretcher.


## 10. Editing MIDI (pp. 252-292)

**Milestones:** C9

**Editor**
- `[ ]` Note grid, time ruler, note ruler C-2 to C8 or tuning pitches or drum pad names, piano ruler [L] → Drum Rack, tuning
- `[ ]` Velocity and Chance lanes, resize, show/hide all, Swap Lane [M]
- `[ ]` Zoom and scroll by rulers, modifiers, double-click fit, selector, Page keys, Z/X [M]
- `[ ]` Expand Clip View to max height [S]
- `[ ]` Grid snap magnetic after first line; offset-preserving snap [M]
- `[ ]` Create clip: double-click slot or track, command, record, capture [S]
- `[ ]` Draw Mode, melodic or pitch-locked, modifier swap, erase behaviour [M]
- `[ ]` Preview switch global, audition on edit; armed plus Preview enables step record [M]
- `[ ]` MIDI import non-destructive, undoable [S]

**Selection and movement**
- `[ ]` Insert marker, time/note selection toggle, boundary jump, Home/End [M]
- `[ ]` Keyboard select nearest, next, same key track, extend, Select All, Invert Selection [M]
- `[ ]` Shift-click piano ruler adds key track [S]
- `[ ]` Find and Select filters: Pitch, Time (with repeat), Chance, Condition, Count (nth, offset, quantized), Duration, Scale, Velocity [L]
- `[ ]` Filters combine, Invert, Select, indicator dot, repeated time selection [S]
- `[ ]` Move by drag or arrows, octave with Shift, modifier copy mid-drag [S]
- `[ ]` Overlap rule: covering start overwrites, covering end shortens [S]
- `[ ]` Edge resize, keyboard length change [S]
- `[ ]` Fit to Time Range [S]
- `[ ]` Note Stretch markers: proportional, pseudo-marker, mirror by crossing, linked envelopes follow [M]
- `[ ]` Deactivate notes [S]
- `[ ]` Split by hold-drag, command, grid-snap modifier [S]
- `[ ]` Chop on grid, count adjust by arrows or drag, power-of-two modifier [S]
- `[ ]` Join same-pitch notes, merges MPE [S] → MPE

**Utilities and expression**
- `[ ]` Transpose, Fit to Scale (nearest, lower on tie), scale-relative Invert, Add Interval [S each]
- `[ ]` Stretch, x2, /2, Duration options, Humanize, Reverse, Legato [S each]
- `[ ]` Quantize four ways; command uses tool settings [S]
- `[ ]` Velocity editing: drag, type, keyboard ±10, fine, Alt-drag on notes, colour saturation [M]
- `[ ]` Velocity Randomize with amount; Ramp start/end [S]
- `[ ]` Velocity Deviation: per-note random range per playback, shaded display, double-click reset [M] → per-note RNG in player
- `[ ]` Draw velocities with grid, individual, line, crescendo [S]
- `[ ]` Release velocity lane [S]
- `[ ]` Chance per note 0-100%, corner triangle, randomize range [M]
- `[ ]` Probability groups: Play All, Play One; markers, shortcut repeats last type, ungroup, status display [M]

**Folding and clips**
- `[ ]` Fold to Notes, global across Set; Drum Rack pad-aware [S]
- `[ ]` Highlight Scale with root emphasis [S]
- `[ ]` Fold to Scale; out-of-scale notes stay visible [S]
- `[ ]` Spelling: flats, sharps, both, Auto by circle of fifths, MIDI numbers [S]
- `[ ]` New clip inherits most recent scale, inactive if disabled [S]
- `[ ]` Crop Clip; Duplicate/Delete/Insert Time inside clip, boundaries unchanged [S]
- `[ ]` Loop duplicate; looped by default [S]

**Multi-clip**
- `[ ]` Up to eight clips shown together; loop bars, resize, select, duplicate [L]
- `[ ]` Focus Mode, toggle and momentary; inactive grey; hover reveal [M]
- `[ ]` Non-focus: edit across clips and loop boundaries, cross-clip paste [L]
- `[ ]` Velocity and Chance apply to foreground clip only [S]
- `[ ]` Shared loop, signature, groove, scale; asterisk on mixed [S]
- `[ ]` Session: looped clips only, realign display, ordered by track then scene [M]
- `[ ]` Arrangement: up to eight tracks, draw across boundaries, half-lit Loop state [M]


## 11. MIDI Tools (pp. 293-329)

**Milestones:** C11

**Framework**
- `[ ]` Transform and Generate panels with tool selector [M]
- `[ ]` Auto Apply default, Apply button, disable restores originals [M]
- `[ ]` Apply shortcut; Reset affects parameters only; undo affects notes only [S]
- `[ ]` Scope: note/time selection or loop; generators replace only where overlapping [S]
- `[ ]` Pitch parameters in scale degrees when scale active [S] → Scale Mode
- `[ ]` Third-party tools from fixed User Library folders or Places; filter tags [M MAX]; a native plug-in API is the non-Max equivalent [L]

**Transformations**
- `[ ]` Arpeggiate: 18 styles, Distance, Steps, Rate, Gate [M] → Arpeggiator logic
- `[ ]` Chop: 2-64 parts, Gaps, 16-part pattern toggles, Emphasis with 2-8x stretch, Variation [M]
- `[ ]` Connect: Spread, Density, Rate, Tie [S]
- `[ ]` Glissando: Start, Curve; pitch-bend ties between notes [M] → MPE
- `[ ]` LFO: Pitch/Slide/Pressure target, Attack/Decay, four shapes, Reseed, Rate 1 to 1/128, Time Shift, Amount, Base [M] → MPE
- `[ ]` Ornament: Flam (Position, Velocity); Grace (High/Low/Same, Position, Velocity, Chance, Amount) [S]
- `[ ]` Quantize: grid or value, triplets, starts/ends, Amount [S]
- `[ ]` Recombine: Position/Pitch/Duration/Velocity; Shuffle then Mirror then Rotate, Rotate on Grid [M]
- `[ ]` Span: Legato, Tenuto, Staccato, Offset, Variation [S]
- `[ ]` Strum: Low, High, Tension, display [S]
- `[ ]` Time Warp: 1-3 breakpoints, Quantize, Preserve Time Range, Include Note End [M]
- `[ ]` Velocity Shaper: envelope, min/max, Loop, Rotate, Division [S MAX]; native reimplementation removes MAX

**Generators**
- `[ ]` Rhythm: Pitch, Steps to 16, Pattern, Density, Step Duration, Split, Shift, Velocity, Accent, Frequency, Offset [M]
- `[ ]` Seed: pitch/duration/velocity ranges, Voices, Density; piano-ruler range pick [S]
- `[ ]` Shape: presets or drawn, range, Rate, Tie, Density, Jitter [M]
- `[ ]` Stacks: Tonnetz chord pads, progression add/remove, Root, Inversion, Duration, Offset [L]
- `[ ]` Stacks custom chord banks: JSON .stacks, double-click loads, tag filter [S; format PROP if interop wanted]
- `[ ]` Euclidean: four voices, Rotation, randomize, pitch/pad, velocity, Steps, Density, Division [M MAX]; native reimplementation removes MAX


## 12. Editing MPE (pp. 330-339)

**Milestones:** A14, C10

- `[ ]` MPE Mode per controller; input channel locked to All [M]
- `[ ]` Five dimensions: Pitch, Slide, Pressure, Velocity, Release Velocity [L] → per-note data model
- `[ ]` Editable on all MIDI clips regardless of source [S]
- `[ ]` Lanes show/hide, resize; Pitch overlaid on notes [M]
- `[ ]` Track meter blue dot for per-note activity [S]
- `[ ]` Selected-note overlay, others dimmed [M]
- `[ ]` Breakpoints: create, delete, drag, hover values, Edit/Add Value, group drag [L]
- `[ ]` Segment drag, inserts edge breakpoints; curved segments [M]
- `[ ]` Grid off by default, separate from other tabs, saved with clip [S]
- `[ ]` Axis lock, fine-drag, drag-over deletes neighbour [S]
- `[ ]` Scale non-Pitch envelopes proportionally; Select All offset [S]
- `[ ]` Envelopes move and stretch with notes [M] → Note Stretch
- `[ ]` Pitch semitone snap modifier [S]
- `[ ]` Pitch hidden under Fold; zoom fits bend range; Clear All Envelopes [S]
- `[ ]` Draw Mode freehand, grid steps, momentary [M]
- `[ ]` Live devices with MPE and Push 2 poly aftertouch [L HW] → instruments
- `[ ]` Plug-in MPE: state in default config, MPE out from plug-ins [M] → VST3/CLAP/AU hosting
- `[ ]` MPE/Multi-channel dialog from External Instrument, mixer I/O, plug-in menu [M]
- `[ ]` Output zones lower/upper, channel count; global channels 1 and 16; one zone per track; zones output-only [M]
- `[ ]` Multi-channel mode with arbitrary range for multitimbral targets [S]


## 13. Converting Audio to MIDI (pp. 340-344)

**Milestones:** C14, A19

- `[ ]` Slice to New MIDI Track: beat resolutions, transients, or Warp Markers; 128-slice cap [L] → Drum Rack, Simpler, Macros
- `[ ]` Slicing presets, factory and User Defaults; factory set is [CONTENT] [M]
- `[ ]` Preserve Warped Timing option [M] → warp engine
- `[ ]` Auto-created track, chromatic staircase clip, one Simpler per chain, pre-mapped Macros [M]
- `[ ]` Resequence by notes or pad swap; group chains with effects [S] → Racks
- `[ ]` Convert Harmony: polyphonic pitch extraction, piano Rack preloaded [XL ML CONTENT]
- `[ ]` Convert Melody: monophonic extraction, synth/piano Macro Rack [L ML-optional CONTENT]
- `[ ]` Convert Drums: onsets, kick/snare/hat classification, Drum Rack [L ML CONTENT]
- `[ ]` Transient markers steer note divisions [S] → transients


## 14. Using Grooves (pp. 345-350)

**Milestones:** A14

- `[ ]` .agr library [S]; format PROP if interop wanted; factory grooves CONTENT
- `[ ]` Drag onto clip; hot-swap during playback [S]
- `[ ]` Audio applied through warping, Warp required [M] → warp
- `[ ]` Groove Pool panel, inactive greyed, load, save, hot-swap [M]
- `[ ]` Parameters real-time: Base, Quantize, Timing, Random (per voice), Velocity -100 to +100 [L] → non-destructive player timing layer
- `[ ]` Global Amount to 130%, appears in Control Bar when used [S]
- `[ ]` Commit: MIDI moves notes, audio creates markers, chooser resets [M]
- `[ ]` Edit by dropping groove to MIDI track [S]
- `[ ]` Extract Groove from audio or MIDI, playing region only, timing and volume [M] → transients
- `[ ]` Quantize-only pattern gives live non-destructive quantize [S]
- `[ ]` Extract chain to groove a single voice [S] → Racks ch. 25


## 15. Using Tuning Systems (pp. 351-359)

**Milestones:** A14

- `[ ]` Scala .scl load [M]; ASCL extension [S PROP, spec is published]; bundled tunings CONTENT
- `[ ]` Per-octave and pseudo-octave scales; description in Info View [S]
- `[ ]` Tunings label, User tag, drag or Enter loads, saved with Set [S]
- `[ ]` All native instruments support tuning [L] → every instrument
- `[ ]` MPE plug-ins and Max instruments need 48-semitone bend [M; Max path MAX]
- `[ ]` Note ruler shows tuning pitches; hover shows pitch and frequency [M]
- `[ ]` Default change keeps note positions; Retune Set option maps by nearest pitch, confirm dialog, may delete or shorten overlaps [M]
- `[ ]` Scale Mode and Use Current Scale disabled while tuning loaded [S]
- `[ ]` Tuning section: Octave, Note, Ref Pitch/Freq; only frequency is audible [S]
- `[ ]` Lowest/Highest note linked [S]
- `[out]` Save .ascl; external tuning website link [S CLOUD for link]
- `[ ]` Delete returns to 12TET [S]
- `[ ]` Per-track Bypass Tuning; Drum Racks auto-bypass [S]
- `[ ]` Controller layouts: All, Black, White, Closest, Custom dialog saved in Set [M]


## 16. Launching Clips (pp. 360-371)

**Milestones:** C7, C8

- `[ ]` Launch settings on Session clips, multi-clip edit [S]
- `[ ]` Launch Modes: Trigger, Gate, Toggle, Repeat at quantize rate [M]
- `[ ]` Legato: inherit play position, works unquantized; dropout risk mitigated by RAM Mode [M] → RAM Mode, streaming preload
- `[ ]` Clip quantize: None, Global, bar and note values; Global shortcut [M]
- `[ ]` Velocity Amount: MIDI velocity to volume, 0-100% [S]
- `[ ]` Nudge Backward/Forward by global quantize [S]
- `[ ]` Mapped scrub control appears in MIDI Map Mode [S]
- `[ ]` Offsets from master clock retained [S]
- `[ ]` Follow Actions on clips and scenes, off by default, toggle shortcut [L]
- `[ ]` Two actions A/B with Chance and linking slider [S]
- `[ ]` Linked (end of clip, loop Multiplier) or Unlinked (Follow Action Time, default one bar) [M]
- `[ ]` Scenes have time only [S]
- `[ ]` Editor marker draggable for Follow Time [S]
- `[ ]` Ten actions: No Action, Stop, Play Again, Previous, Next (wraps), First, Last, Any, Other, Jump with target [M]
- `[ ]` No Action blocks subsequent actions, Stop overrides loop [S]
- `[ ]` Groups are adjacent slots, separated by empties [S]
- `[ ]` Striped launch button [S]
- `[ ]` Timing bypasses Global quantize, honours specific clip quantize [S]
- `[ ]` Global enable button, greyed when unused [S]
- `[ ]` Scene actions take precedence once fired; clip actions continue until then [M]
- `[ ]` Create Follow Action Chain, non-contiguous selection allowed [S]


## Key dependency chains
- Tempo map and sample-accurate clock → warping → grooves, Legato, Auto-Warp, Splice preview.
- Clip model with undo → all editors, multi-clip, linked tracks.
- Per-note data model → MPE editor, Join, Glissando, LFO, tuning.
- Simpler, Drum Rack, Macros → Slice to MIDI, browser load, Convert commands.
- Transient detector → Beats mode, audio quantize, Extract Groove, Convert.
- Offline clip renderer → Consolidate, Crop, Reverse, Export, Capture workflows.


## Manual inconsistencies to resolve before specifying
- Humanize range is up to half a grid division on p. 224 but a quarter either way on p. 273.
- Import format lists differ between p. 122 and p. 240 (AAC, Ogg FLAC).
- P. 197 still describes tempo logging via scene names, while pp. 191-192 use dedicated controls.


## The 15 features that most define the Live workflow
1. Session grid: clips, scenes, random-access launching, Capture and Insert Scene
2. Shared tracks across Session and Arrangement, with Session override and Back to Arrangement
3. Global and clip launch quantization, with Trigger/Gate/Toggle/Repeat
4. Real-time warping and warp modes
5. Auto-Warp, transients, and Warp Markers
6. Non-destructive clip model: sample reference plus region, loop, gain, pitch
7. Recording Session performance into the Arrangement as clips and automation
8. Session recording with overdub and quantized close-and-launch, without stopping
9. Follow Actions combined with Legato
10. Selection-based Arrangement editing: Time commands, Split, Consolidate
11. MIDI Note Editor with fold, probability, velocity deviation, multi-clip editing
12. Scale awareness across clips, tools, and devices, with MIDI Tools live-apply
13. Groove Pool non-destructive timing
14. Browser with tempo-matched preview, tags, and hot-swap
15. Project, Collect, and Live Clip portability

## 17. Routing and I/O (372-393)

**Milestones:** A5, A15

- `[ ]` In/Out section with type and channel chooser pairs for input and output; show/hide; applies across multi-selected tracks. M
- `[ ]` Monitor modes In, Auto, Off. Delete or Return to Default resets to Off on audio tracks and Auto on MIDI tracks. In state tints the Activator blue. S
- `[ ]` Keep Monitoring Latency in Recorded Audio, on by default for In and Auto; right-click toggle. M. Depends on PDC.
- `[ ]` External audio I/O: per-channel meters with overload, Configure shortcut, mono/stereo pair configuration, channel renaming stored per audio device. M
- `[ ]` Mono input records mono files; device chains are always stereo; mono to stereo duplicates; stereo to mono output sums with -6 dB. S
- `[ ]` External MIDI input: one port or All Ins, one channel or All Channels, activity meters. S
- `[ ]` Per-port switches: Track, Sync, Remote, separately for input and output. M
- `[ ]` Sync: MIDI Clock in and out, MIDI Timecode in. L
- `[ ]` Sync output options: delay in ms; clock type Song (SPP and Continue) or Pattern (Start at next bar). S
- `[ ]` Hardware Resync: Stop and Start (default), Start Only, Don't; Control Bar button when enabled. S
- `[ ]` Remote input for mappings and clip triggering; output for LED and motor-fader feedback. Mapped messages do not reach tracks. M. [H] for feedback.
- `[ ]` Computer MIDI keyboard: home row white keys from C3, top row black keys, Z/X octave, C/V velocity in steps of 20; steals single-key shortcuts, Shift reaches them. S
- `[ ]` MIDI out to external synths by port and channel. S
- `[ ]` Control Bar MIDI LEDs, three in/out pairs: sync, remote, track. Sync pair shows only when a sync source is enabled. S
- `[ ]` Resampling input type records Main output; the recording track's own output is excluded. Files go to Samples/Recorded, or the Temporary Folder until saved. M
- `[ ]` Track-to-track routing from either end (output to track, or input from track). L
- `[ ]` Tap points Pre FX, Post FX, Post Mixer, each with defined solo behaviour. L
- `[ ]` Tap points for every Rack chain and Drum Rack return chain. M. Depends on Racks.
- `[ ]` Sends Only output type. S
- `[ ]` MIDI output to another track, targeting Track In or a specific device directly. M
- `[ ]` No Input type hides Arm. S
- `[ ]` Multi-output instruments appear as sources. Tapping an Impulse slot removes it from Impulse's mix; plug-ins do not do this. M
- `[ ]` Multi-timbral plug-ins: MIDI by channel, aux outputs by track or via External Instrument. M
- `[ ]` Plug-in sidechain inputs as routing targets; built-in devices have their own sidechain choosers. M
- `[ ]` MIDI tapped from another track is taken after MIDI effects and before the instrument. S


## 18. Mixing (394-411)

**Milestones:** A1, A4, A5

- `[ ]` Mixer in both views, plus Arrangement Track Controls; component visibility menu. M
- `[ ]` Peak plus RMS meters, showing input while monitoring. S
- `[ ]` Pan modes Stereo and Split Stereo; click-triangle reset. S
- `[ ]` Activator, Solo, Arm; selection-wide changes; Exclusive Solo and Exclusive Arm options; Ctrl/Cmd for multiple. S
- `[ ]` With Exclusive Arm, inserting an instrument auto-arms. S
- `[ ]` Resizable mixer: taller meters, ticks, numeric volume, resettable peak, dB scale on wide tracks. M
- `[ ]` 32-bit float engine; clipping only matters at outputs, Main and file writes. S
- `[ ]` Track creation by menu, shortcut, double-click or Enter on browser items, or drops of files and clips. M
- `[ ]` Rename with Tab to next track; # auto-numbering with extra # for zero padding; info text. S
- `[ ]` Reorder and resize; multi-select keeps relative value offsets; non-adjacent drag collapses, Ctrl+arrows does not. M
- `[ ]` Drag track to browser folder saves a Set, with Collect Files on Export behaviour. M
- `[x]` Group Tracks: nesting; auto-routing unless custom; fold; Ungroup; deleting removes contents. M
- `[~]` Group Arrangement overview; Session group slots launch, stop and select contained clips. M
- `[~]` Assign Track Color to grouped tracks and clips, per view; half-lit Solo for nested solo. S
- `[x]` Return tracks: multiple; Pre/Post toggle; feedback sends disabled by default, with Enable Send and Enable All Sends. M
- `[x]` Main track: exactly one, hosts effects. S
- `[ ]` Crossfader: A/B assign on all tracks including returns; 7 curves; left, centre and right mappable, with toggle and snap-back key behaviour; automatable. M
- `[x]` Solo in Place option; returns stay audible only with it on. S
- `[x]` Soloing a return mutes other mains but keeps send audio. S
- `[ ]` Cue mode: separate Main and Cue outputs, Cue Volume, headphone switches, browser preview goes to Cue. M. Needs 4 outputs.
- `[~]` Track Delay in ms or samples; unavailable when PDC is off. M
- `[ ]` Performance Impact meters: 6 segments per track, relative to Set. M. Needs per-track DSP timing.


## 19. Recording New Clips (412-424)

**Milestones:** A11

- `[ ]` Defaults: audio from mono ext input 1 or 2; MIDI from all active inputs. S
- `[ ]` Arm in either view; exclusive unless modified; arming selects the track and locks a native control surface. S. [H] for lock.
- `[ ]` Arrangement Record; Start Playback with Record preference, inverted by Shift. M
- `[ ]` MIDI Arrangement Overdub (MIDI only). M
- `[ ]` Punch-In and Punch-Out tied to Arrangement Loop edges. M
- `[ ]` Loop recording keeps all passes in one sample; loop brace selects last pass; undo unrolls. L
- `[ ]` Session recording into slots, single-slot or Session Record across armed tracks; pressing again goes straight to loop playback. L. Depends on launch quantization.
- `[ ]` New button: stops armed clips, finds or creates a scene; available only through key/MIDI mapping. S
- `[ ]` Start Recording on Scene Launch option. S
- `[ ]` MIDI pattern overdub in looping clip; toggle overdub; Alt-double-click creates, arms and launches. M
- `[ ]` Undo removes last overdub take. M
- `[ ]` Step recording: hold notes, right arrow advances by grid, extends held notes, left arrow deletes; mappable. M
- `[ ]` Tempo may change during recording, with sync retained. L. Depends on warp.
- `[ ]` Metronome: count-in, sound, beat division (Auto follows denominator), fallback when division stops fitting, Enable Only While Recording, respects punch-in. M
- `[ ]` Record Quantization: separate undo step in Arrangement; locked mid-record; immediate in looped overdub. M
- `[ ]` Count-in displayed in blue as negative bars-beats. S
- `[ ]` File Type, Bit Depth and default Warp Mode preferences. S
- `[ ]` Temporary Folder until saved. S
- `[ ]` All record controls are mappable. S
- `[ ]` Capture MIDI: constant retrospective buffer on armed and monitored tracks. L
- `[ ]` Capture on empty stopped Set: detects tempo (80-160 BPM), loop and grid; starts transport; keeps pre-phrase notes before start marker; single-note special case. L
- `[ ]` Capture with running transport or existing clips: keeps tempo, detects phrase, overdubs a playing clip. M
- `[ ]` Capture button on Push. [H]


## 20. Bounce to Audio (425-430)

**Milestones:** A12

- `[ ]` Bounce Track in Place replaces the track in both views; supersedes Freeze and Flatten. L. Depends on offline render.
- `[ ]` Bounce to New Track for clips or time selection; mutes source; one new track per source; Ctrl/Cmd+B. M
- `[ ]` Track bounces are post-FX and pre-mixer; mixer settings are copied. M
- `[ ]` Session versus Arrangement results differ. S
- `[ ]` Naming suffix (Bounce), colour inherited, files in Samples/Processed/Bounce. S
- `[ ]` Bounce Group in Place and to New Track are post-mixer, include internal sends, exclude Main processing. Tracks routed out may produce silence. L
- `[ ]` Paste Bounced Audio renders source at paste time; converts an empty MIDI track to audio; can target take lanes. M


## 21. Comping (431-436)

**Milestones:** C6

- `[ ]` Take lanes on audio and MIDI tracks; Main lane; only audible by Audition. L
- `[ ]` Show/hide; hidden in Automation Mode; left arrow folds. S
- `[ ]` Insert, duplicate, delete, Delete All, Delete All Unused. S
- `[ ]` Lane resize, reorder, rename with Tab; lanes exposed through Live API. M. [X] for API.
- `[ ]` Auto-created while recording; new lane per overlapping pass; reuse rule after punch-in; last take copied to Main. L
- `[ ]` Random take colour option. S
- `[ ]` Drag samples or MIDI to lanes; Ctrl/Cmd spreads across tracks. S
- `[ ]` Audition one lane per track (T). M
- `[ ]` Copy selection to Main (Enter); Ctrl/Cmd+Up/Down swaps take; Draw Mode single-gesture comp; click lane to replace selection. L
- `[ ]` Main-lane clips are independent copies; take clips edit normally and can move to Session. M
- `[ ]` Create Fades on Clip Edges gives 4 ms crossfades. S
- `[ ]` Source highlights and desaturated unused takes; drag highlight edge moves split point. M


## 22. Stem Separation (437-444)

**Milestones:** A19

- `[ ]` Local model separates to Vocals, Drums, Bass, Others. XL. [ML] [P]. Open models are a realistic replacement; check licence.
- `[ ]` Separate Stems from browser, clip, or Arrangement time selection. M
- `[ ]` Dialog: stem toggles; Merge to Single Track for 2 or 3 stems only; quality mode. S
- `[ ]` High Speed is a single pass; High Quality uses dedicated passes per stem, with Others as the remainder. L. [ML]
- `[ ]` Including Others forces all passes; unrequested stems are still saved. S
- `[ ]` Result: Group Track of coloured tracks; single stem gets one track; source clip deactivated. M
- `[ ]` Clip markers and warped loop define the processed region. M. Depends on warp.
- `[ ]` Track effects move to the group; automation copied; clip envelopes copied to stems. M
- `[ ]` Files in Samples/Processed/Stems at 44.1 kHz and source bit depth. S
- `[ ]` Empty Set adopts source tempo if auto-warp is on. S
- `[ ]` GPU only on Apple silicon; CPU elsewhere. L. [ML]


## 23. Instruments and Effects (445-478)

**Milestones:** K3, K7, A4, C12

- `[ ]` Three device classes with MIDI, instrument, audio order enforced. M
- `[ ]` Device View; stacked Clip and Device Views; fold. M
- `[ ]` Insert by double-click, Enter or drag; sample drop on MIDI device area creates Simpler. M
- `[ ]` Reorder, cross-track drag, cut/copy/paste/duplicate; paste position rules; live edits without audio dropouts. L. Needs lock-free graph swap.
- `[ ]` Level meters between devices. S
- `[ ]` Title bar: Activator (off means no CPU), hot-swap, save, options, scale toggle, expand toggles (above-device view and in-device view). M
- `[ ]` A/B device states: built-ins only; Copy A to B; switch with P; automation disabled on switch and re-enabled per parameter; copy or save flattens the selected state to both. M. API [X].
- `[ ]` Presets per device in browser. M. [O] for factory set.
- `[ ]` Live file types .adg, .adv, .amxd can be dropped. Read support would be L. [P] format, [X] for .amxd.
- `[ ]` Hot-Swap presets (Q), same device type only; Options.txt auto-load flag; several exit paths. M
- `[ ]` Hot-Swap samples in Drum Rack pads, Drum Sampler, Impulse, Sampler, Simpler. S per target. Depends on instruments.
- `[ ]` Save Preset to User Library, including Info text; Collect Files prompt. M
- `[ ]` Default presets per device; name must match. S
- `[ ]` Default plug-in configurations save parameter list only, not values. S
- `[ ]` Track defaults for audio and MIDI. S
- `[ ]` Sample dropping defaults: On Device View, On Drum Rack; Save as Default Pad; last added wins. S
- `[ ]` Slicing defaults, shown in Slice dialog. S. Depends on slicing.
- `[ ]` Audio-to-MIDI defaults: Drums, Harmony, Melody folders; last added wins. S. Depends on conversion.
- `[ ]` Project-specific Defaults folder overrides User Library. S
- `[~]` Automatic device and plug-in delay compensation, including returns; Options toggle. XL (with monitoring and tap-point cases)
- `[ ]` Reduced Latency When Monitoring option. M
- `[~]` Tempo-synced devices after latent devices may drift; PDC raises CPU. Document.


## 24. Using Plug-Ins (479-490)

**Milestones:** A17

- `[ ]` VST2, VST3, AU2, AU3 (AU on macOS only). XL overall. VST2 SDK licensing is restricted (my note, not in the manual).
- `[ ]` Skip scan at launch with modifier; Activate flow; Rescan; modifier forces clean database rebuild. M
- `[ ]` Crash-safe scanning: reports culprit, offers rescan or disable, auto-disables after repeat. L. Needs out-of-process scan.
- `[ ]` Browser separates instruments from effects; AU presets only. M
- `[ ]` Generic panel: sliders for up to 64 parameters; empty panel above that. M
- `[ ]` X-Y field for two parameters. S
- `[ ]` Floating editor windows: Auto-Open, Multiple Windows (modifier overrides), Auto-Hide by track; hide-all shortcut; track name in title. L
- `[ ]` Configure Mode: add by touching, reorder, delete, warns if parameter is automated or mapped; unique per instance. M
- `[ ]` Implicit panel additions via temporary chooser entries, recording changes, or mapping modes. M
- `[ ]` Plug-ins behave as native devices for mapping, envelopes, moves, multi-I/O. L. Depends on host.
- `[ ]` Sidechain block: source, Gain, Mix, Mute. M
- `[ ]` VST folders: custom folder, Windows registry path, macOS system folders, aliases followed. S
- `[ ]` VST per-instance preset bank; rename; load and save .fxp/.fxb style files. M
- `[ ]` AU: quality modes only in plug-in UI; AU presets on disk, some read-only. M


## 25. Racks (491-510)

**Milestones:** A8, C15

- `[ ]` Four types: Instrument, Drum, Audio Effect, MIDI Effect, with type rules. XL as a system.
- `[ ]` Parallel chains, summed; unlimited nesting; Group, Group to Drum Rack, Ungroup; bracket UI. L
- `[ ]` View column; fold; hierarchical device navigator on selector context menu. M
- `[ ]` Chain List: Activator, Solo, Hot-Swap, volume, pan; multi-select; drop area; arrow-key flip; rename, colour, info text; chain presets; drag across tracks. L
- `[ ]` Auto Select. S
- `[ ]` Zones: main range plus fade range, drag-edited. L
- `[ ]` Key zones. M. Velocity zones (1-127). M. Fades attenuate note velocity.
- `[ ]` Chain Select zones 0-127, default length 1 at value 0; Filters MIDI Ctrl option. M
- `[ ]` Chain Select fades attenuate output in audio Racks; without fades tails ring out. M
- `[ ]` Drum Rack chain controls: Receive note, Play note, Choke (16 groups), Preview; All Notes disables Play and Choke. L
- `[ ]` Up to 6 return chains; post-fader sends; return output to Rack or Set returns. M
- `[ ]` Pad View: 128 notes, scroll by 16 or row; drag anything to a pad. L
- `[ ]` Sample drop creates Simpler, replaces only source; multi-drop maps chromatically; modifier layers to nested Rack. M. Depends on Simpler.
- `[ ]` Pad swap and layer; shared Receive notes create Multi pad with reduced controls. M
- `[ ]` D toggles Hot-Swap target to last pad. S
- `[ ]` Control surface pads follow scroll. M. [H]
- `[ ]` Macros: up to 16, 8 by default, visible count saved. M
- `[ ]` Macro Map Mode with Mapping Browser, Min/Max, inverted range; mapped parameter locked except clip envelopes; auto naming and units; rename, colour, info text. L
- `[ ]` Rand button; Exclude Macro from Randomization; Volume macros excluded by default in Instrument Rack presets. S
- `[ ]` Macro Variations: store, launch, overwrite, rename, duplicate, delete; Exclude Macro From Variations. M
- `[ ]` Rack mixer inside Session View, with bidirectional sync and multi-select (mixer side only). L
- `[ ]` Extract chains; Drum return creates return track; drum chain from mixer carries its MIDI notes. L


## 26. Automation (511-532)

**Milestones:** A2, C3

- `[~]` Breakpoint envelopes on nearly all mixer and device controls including tempo; LED states (automated, modulated, overridden). L
- `[~]` Automation Arm. S
- `[ ]` Session clip automation recording, armed tracks or All Tracks setting. L
- `[ ]` Session-to-Arrangement capture becomes track automation; copying clips transfers automation. L
- `[x]` Arrangement automation records on all tracks. M
- `[~]` Overwrite: mouse is touch; hardware is latch; first pass in a clip always latch. M
- `[~]` Override by manual change; Re-Enable global button and per parameter; relaunching clip restores. M
- `[ ]` Clip Envelope Editor with Automation toggle and Device/Control choosers; Show Adjusted Only merges choosers. M
- `[ ]` Automation Mode (A), main lane, Add Lane; hide additional lanes; duplicates limited to two; Add Lane for Each Automated Envelope; remove with modifiers; arrow navigation. L
- `[ ]` Show Automation and Show in New Lane from parameter context menu. S
- `[ ]` Dashed line for unautomated control; clicking a control shows its envelope. S
- `[ ]` Breakpoint creation: click line, double-click off-line, Add Value, snap bypass. M
- `[x]` Stepped envelopes for discrete parameters. S
- `[ ]` Draw Mode (B, momentary hold), grid-width steps; Snap to Grid off for smooth. M
- `[ ]` Shapes: sine, triangle, saw, inverse saw, square; ramps relative to neighbours; ADSR; only square for discrete. M
- `[ ]` Edit breakpoints and segments, selections, fine drag, axis constrain, drag-through overwrite only on drop. L
- `[ ]` Curved segments with Alt, straighten with Alt-double-click. M
- `[ ]` Edit Value, typed, relative across selection. S
- `[ ]` Stretch and skew handles, mirrored with modifier. M
- `[ ]` Simplify Envelope. M
- `[ ]` Delete Automation, Clear Envelope, Clear All of device, Clear All in Set. S
- `[ ]` Tempo automation with Min/Max display range, which also bounds MIDI control. S
- `[ ]` Lock Envelopes to timeline. S
- `[ ]` Cut, copy, duplicate, delete inside time selections, across lanes, even across parameters. M


## 27. Clip Envelopes (533-545)

**Milestones:** A3

- `[ ]` Envelope tab: audio clips have Clip, effects, Mixer; MIDI clips have MIDI Ctrl, devices, Mixer. M
- `[ ]` Session clips have Automation and Modulation modes; Arrangement clips modulation only. M
- `[ ]` Blue versus red colours and knob ring display. S
- `[ ]` MIDI Envelope Auto-Reset option. S
- `[ ]` Non-destructive, computed in real time. Architecture requirement.
- `[ ]` Transposition envelope additive, clamped to ±48. M. Depends on warp.
- `[ ]` Clip Gain envelope relative percentage. S
- `[ ]` Sample Offset envelope in Beats mode only, ±8 sixteenths. L. Depends on warp.
- `[ ]` Clip as template: dropping a sample keeps settings. S
- `[ ]` Modulation composes with automation; automation sets range. L
- `[ ]` Modulated Track Volume dot; Sends relative; Pan intensity scales with knob position. M
- `[ ]` Device parameter modulation relative. M. Depends on parameter system.
- `[ ]` MIDI CC envelopes up to 119, including imported data. M
- `[ ]` Unlink: local loop, region, start marker, per envelope, odd lengths. L
- `[ ]` Linked envelopes follow Warp Markers, which are editable in the envelope view. M


## 28. Working with Video (546-551)

**Milestones:** C18

- `[ ]` .mov import; video only in Arrangement, Session treats as audio. L. Needs a decoder.
- `[ ]` Sprocket clip look; trim as audio. S
- `[ ]` Consolidate, Reverse, Crop replace with audio internally. S
- `[ ]` Floating always-on-top Video Window; geometry not per Set; full-screen, second monitor. M
- `[ ]` Partial tracks: black or silence in gaps. S
- `[ ]` Tempo Leader defaults for video and only the bottom-most playing leader counts. M. Depends on warp.
- `[ ]` Warped non-leader video plays warped. M
- `[ ]` Warp marker drag scrubs the video frame; QuickTime markers displayed. M
- `[ ]` Pre-roll workflow via Start Marker and edge drag; Export Audio/Video duration follows selection. M


## 29. Audio Effect Reference (552-690)

**Milestones:** B0, B3 to B9


**Shared components to build first** (reuse saves more than any single effect):
- `[ ]` Tempo-synced rate modes. S
- `[ ]` Stereo LFO with Phase and Spin. M
- `[ ]` Envelope follower. S
- `[ ]` Sidechain block with routing, gain, mix, EQ, listen. M
- `[ ]` Oversampling path. M
- `[ ]` Spectrum and scope widgets for egui. M
- `[ ]` Equal-loudness dry/wet. S
- `[ ]` Scale-awareness adapter. S

| Effect (pages) | Modes and options | Main parameter groups | DSP needed | Effort and tags | Status |
|---|---|---|---|---|---|
| Amp (552-554) | 7 models; mono or Dual | Gain, Volume, tone stack, Presence | Nonlinear amp modelling, interactive EQ | XL [P] | `[ ]` |
| Auto Filter (554-562) | 10 types; slopes 6-48 dB; circuits SVF, DFM, MS2, PRD | Filter, LFO, envelope, drive, soft clip, SC filter | ZDF filters, ladder and Sallen-Key models, vowel, resampling, comb | L [P] | `[ ]` |
| Auto Pan-Tremolo (562-566) | Panning, Tremolo; Harmonic (600 Hz split), Vintage | LFO, shape, attack, dynamic rate | LFO amplitude, crossover, transient detect | M | `[ ]` |
| Auto Shift (566-573) | Quantizer or MIDI; Mono or Poly to 8; Live Mode; pitch ranges | Input, correction, scale, MIDI mods, LFO, formant, vibrato | Mono pitch detection, formant-preserving shift, polyphonic voices | XL [P] | `[ ]` |
| Beat Repeat (574-575) | Variation modes; Mix, Insert, Gate | Interval, Offset, Chance, Gate, Grid, Pitch decay, filter | Tempo-locked buffer, resampling | M | `[ ]` |
| Cabinet (576-578) | 5 speakers; 3 mic positions; dynamic or condenser; mono or Dual | Speaker, mic | IR or filter modelling; needs own measurements | L [P] | `[ ]` |
| Channel EQ (578-579) | HP 80 toggle | Low, Mid sweep, High | Adaptive shelf and peak, LP-combined high | S | `[ ]` |
| Chorus-Ensemble (580-583) | Chorus, Ensemble, Vibrato | Delay, taps, width, rate, feedback, warmth, HPF | Modulated delay lines, 3-phase | M | `[ ]` |
| Compressor (584-589) | Peak, RMS, Expand; Lin, Log; lookahead 0/1/10 ms | Threshold, ratio, knee, attack, release, sidechain, 3 views | Feed-forward dynamics, lookahead | M | `[ ]` |
| Corpus (589-595) | 7 resonators; Eco to High | Decay, material, tuning, LFO, filter, MIDI sidechain | Modal and waveguide physical models | XL [P] | `[ ]` |
| Delay (595-598) | Sync, Time; Repitch, Fade, Jump; Ping Pong; Freeze | Times, offset, feedback, BP filter, LFO | Interpolated delay, crossfade, Hi-Quality tape smoothing | M | `[ ]` |
| Drum Buss (599-600) | Soft, Medium, Hard | Trim, comp, drive, crunch, damp, transients, boom | Fixed comp, waveshape, transient shaper, resonant LF with note snap | L [P] | `[ ]` |
| Dynamic Tube (601-602) | Tubes A, B, C; Hi-Quality | Drive, bias, envelope, tone | Asymmetric waveshaper, oversampling | M [P] | `[ ]` |
| Echo (602-606) | Stereo, Ping Pong, Mid/Side; sync modes; reverb pre, post, feedback | Delay, filter, modulation, gate, duck, noise, wobble | Delay, LFO and envelope, tape artefacts, small reverb, tunnel UI | L | `[ ]` |
| EQ Eight (606-608) | Stereo, L/R, M/S; 8 responses; Adaptive Q; audition; oversampling; expanded view | 8 bands, scale, analyzer | Biquads, FFT analyzer | M | `[ ]` |
| EQ Three (608-609) | 24 or 48 dB | 3 gains to -inf, crossovers, LEDs | Analog-style crossover with deliberate colouring | S [P] | `[ ]` |
| Erosion (609-610) | Sine to noise blend; Legacy for pre-12.4 Sets | Freq, amount, width, stereo | Short delay modulated by sine and filtered noise | S | `[ ]` |
| External Audio Effect (610-611) | ms or samples latency | Out, In, gains, invert, dry/wet | Hardware insert, manual PDC | M [H] | `[ ]` |
| Filter Delay (612) | 3 taps; sync | Filters X-Y, time, feedback, pan, dry | Delay plus filters in feedback | S | `[ ]` |
| Gate (613-615) | Flip; lookahead 0/1/10 ms | Threshold, return, attack, hold, release, floor, sidechain | Hysteresis gate, lookahead | S | `[ ]` |
| Glue Compressor (615-617) | Auto release; soft clip; oversampling | Threshold, ratio, attack, release, range, makeup, sidechain | VCA bus model, dual-time release | L [P] | `[ ]` |
| Grain Delay (618-619) | Sync; X-Y assign | Delay, spray, frequency, pitch, random, feedback | Granular delay | M | `[ ]` |
| Hybrid Reverb (619-625) | Serial, Parallel, Algorithm, Convolution; 5 algorithms; vintage presets | Input, IR browser, envelope, algo, 4-band EQ, output | Partitioned convolution, FDN, shimmer, velvet noise, degrader | XL [O] for IRs [P] | `[ ]` |
| Limiter (626-627) | Standard, Soft Clip, True Peak; L/R, M/S; Maximize | Ceiling, link, release, lookahead 1.5/3/6 ms | Lookahead peak limiter, oversampled true peak | L | `[ ]` |
| Looper (628-631) | Tempo control 3; song control 3; monitor 4; multi-purpose button; ×2, ÷2 | Transport, speed, reverse, feedback | Sync buffer, tempo estimate, device-as-routing-target, import and export | L. Link touches [C] | `[ ]` |
| Multiband Dynamics (632-636) | RMS, Peak; soft knee | 3 bands, above and below thresholds, ratios, time, sidechain | Linkwitz-Riley split, up and down dynamics, custom block UI | L | `[ ]` |
| Overdrive (637) | none | BP X-Y, drive, tone, dynamics | Pedal waveshaper | S [P] | `[ ]` |
| Pedal (638-640) | Overdrive, Distortion, Fuzz; Sub; Hi-Quality | Gain, adaptive 3-band, mid switch | Pedal models | M [P] | `[ ]` |
| Phaser-Flanger (641-644) | Phaser, Flanger, Doubler | Notches, center, spread, 2 LFOs, envelope, Safe Bass | Allpass cascade, modulated delay | M | `[ ]` |
| Redux (644-645) | Pre and Post filter; DC Shift | Rate, jitter, bits, shape | Sample-hold, bit reduction, companding curve | S | `[ ]` |
| Resonators (646-647) | Mode A, B; scale aware; tuning systems | Input filter, 5 notes, decay, color | Resonant comb bank | M | `[ ]` |
| Reverb (647-651) | Smooth; Density; Freeze, Flat, Cut | Input filter, ER, diffusion, chorus, size, decay | Algorithmic FDN | L | `[ ]` |
| Roar (651-660) | 7 routings; 12 shapers; 9 filters; 5 feedback modes | Input, stages, mod matrix (2 LFO, Env, Noise), feedback, comp, MIDI sidechain | Waveshapers, dispersion, feedback routing, matrix UI | XL [P] | `[ ]` |
| Saturator (660-663) | 8 curves; Post Clip; Hi-Quality; Pre-DC | Drive, color filters, waveshaper params | Waveshapers, mirrored EQ | M | `[ ]` |
| Shifter (663-668) | Pitch, Freq, Ring; Internal or MIDI | Tuning, delay, LFO, envelope, glide | Pitch shift window, Hilbert shift, ring mod | L | `[ ]` |
| Spectral Resonator (668-673) | Internal or MIDI; Mono or Poly to 16; 4 mod modes | Freq, stretch, shift, decay, damp, harmonics, unison | STFT partial resonance, spectrogram | XL | `[ ]` |
| Spectral Time (673-676) | Manual, Retrigger (Onsets, Sync); Crossfade, Envelope; effect order | Freezer, spectral delay tilt, spray, mask, resolution; Zero Dry Signal Latency | STFT freeze, up to 8 stacked; onset detect; per-bin delay | XL | `[ ]` |
| Spectrum (677-678) | Linear, log, semitone; Auto, Range; expanded | Block, channel, refresh, avg, max | FFT analysis (no audio change) | S | `[ ]` |
| Tuner (678-683) | Classic (Target, Strobe), Histogram; Hz or cents; note spelling | Reference 410-480 Hz | Mono pitch detection (no audio change) | M | `[ ]` |
| Utility (684-685) | Channel select; Width or Mid/Side | Phase, gain, balance, mute, DC, Bass Mono with audition | M/S matrix, crossover, DC filter | S | `[ ]` |
| Vinyl Distortion (686-687) | Soft, Hard; stereo or mono pinch | Tracing, pinch, crackle | Even and odd harmonic shaping, noise | S [P] | `[ ]` |
| Vocoder (687-690), not in Intro or Lite | Carrier Noise, External, Modulator, Pitch Tracking; Precise, Retro; Mono, Stereo, L/R | Bands, range, BW, gate, depth, envelope, formant, unvoiced | Filterbank, pitch tracker, unvoiced detection | L | `[ ]` |

**Dependencies between effects**
- `[ ]` Amp and Cabinet are designed as a pair.
- `[ ]` All MIDI-sidechain effects (Auto Shift, Corpus, Roar, Shifter, Spectral Resonator) need MIDI routing from another track at Pre FX, Post FX or Post Mixer.
- `[ ]` All scale-aware effects need the clip scale.
- `[ ]` Pitch detection is shared by Auto Shift, Tuner and Vocoder.
- `[ ]` STFT infrastructure is shared by Spectral Resonator and Spectral Time.


## 30. MIDI Effect Reference (691-703)

**Milestones:** B1

All pitch-related controls support scale degrees through Use Current Scale. Editions are not stated.

| Effect (pages) | Modes and options | Main parameter groups | Technique | Effort | Status |
|---|---|---|---|---|---|
| Arpeggiator (691-693) | Styles incl. Play Order, Chord Trigger, 3 random; Sync or Free; Retrigger Off, Note, Beat | Rate, gate, distance, steps, offset, groove, repeats, velocity slope, hold | Note scheduler, groove lookup | M | `[ ]` |
| CC Control (694-695) | 2 pages; Learn; Send all | Mod Wheel, Pitch Bend, Pressure, Custom A toggle, B-M assignable | CC emission, merges with existing automation | S. Push display [H] | `[ ]` |
| Chord (695-696) | Learn; Velocity or Chance sliders | 6 shifts ±36 (no duplicates), strum to 400 ms, tension, crescendo; duplicate-strum and per-note options | Note generation, MPE forwarding | M | `[ ]` |
| Note Length (697-698) | Note On or Off trigger; Latch | Gate, length; release vel, decay, key scale | Note-off scheduling | S | `[ ]` |
| Pitch (698-699) | Block, Fold, Limit | Pitch ±128 (or ±30 degrees), step buttons, lowest, range | Remap | S | `[ ]` |
| Random (699-700) | Random or Alt; Add, Sub, Bi | Chance, choices, interval | Seeded RNG, round robin | S | `[ ]` |
| Scale (701-702) | User scale; Fold | 13×13 matrix, base, transpose, lowest, range | Lookup table, matrix UI | S | `[ ]` |
| Velocity (702-703) | Clip, Gate, Fixed; Velocity, Rel. Vel., both | Curve, lowest, range, outputs, random, drive, compand | Curve function, curve UI | S | `[ ]` |


## Manual inconsistencies to confirm
- p.374: Auto is called the default monitor mode, but reset gives Off on audio tracks.
- p.561: the Auto Filter sidechain filter list names band-pass, but the text then describes notch.
- p.425 says bounce supersedes Freeze and Flatten, while pp.411 and 451 still recommend Freeze Track.


## The 15 features that most define the Live workflow
1. Device chains with live, glitch-free editing (ch. 23)
2. Racks with parallel chains and nesting (ch. 25)
3. Macros with Variations and randomization (ch. 25)
4. Drum Rack with Pad View and choke groups (ch. 25)
5. Clip envelopes unlinked from clips (ch. 27)
6. Modulation composing with automation (ch. 27)
7. Automation override and Re-Enable (ch. 26)
8. Session slot recording and pattern overdub (ch. 19)
9. Capture MIDI (ch. 19)
10. Internal routing with tap points, including Rack chains (ch. 17)
11. Resampling and Bounce in Place (chs. 17, 20)
12. Take lanes and comping (ch. 21)
13. Hot-Swap presets and samples (ch. 23)
14. Device delay compensation (ch. 23)
15. Scale awareness across effects (chs. 29, 30)


## Counts
- **Audio devices: 42** (sections 29.1-29.42). Of these, Spectrum and Tuner are measurement tools that do not alter audio, and External Audio Effect is a hardware insert. That leaves 39 that process audio internally.
- **MIDI effects: 8.**

## 31. Instrument Reference (704-823)

**Milestones:** B0, B9 to B12


**Shared building blocks (build these first):**
- `[ ]` Voice allocator with polyphony, stealing, retrigger, legato, unison, glide [M]
- `[ ]` Parameter modulation bus and matrix, additive and multiplicative [L]
- `[ ]` Loopable envelopes with slopes and Loop/Beat/Sync/Trigger modes [M]
- `[ ]` Tempo-synced LFOs [S]
- `[ ]` MPE routing: pressure, slide, per-note bend [M]
- `[ ]` Filter library: Clean, OSR, MS2, SMP, PRD circuits, 12/24 dB, Morph, drive. Used by Operator, Sampler, Simpler, Wavetable; Drift and Meld use subsets [L] {PM: Cytomic-modelled, needs own models}
- `[ ]` Scale awareness from global Scale Mode [S]

**Analog (704-713)** [L] {PM}
- `[ ]` Edition not stated. Virtual analog by real-time circuit-equation solving (with AAS); no samples or wavetables.
- `[ ]` 2 oscillators (sine/saw/rect/noise, PW, sub or sync, pitch env, key scaling); noise with colour LP.
- `[ ]` 2 multimode filters (LP/BP/notch/HP/formant, 2nd/4th order, series/parallel, Follow, sym/asym drive); 2 amps.
- `[ ]` 4 ADSRs (AD-R, ADR-R, ADS-R loops, Free, Legato, S.Time, velocity); 2 LFOs.
- `[ ]` Global: vibrato with Error, unison 2/4, const/prop glide, 4 Quick Routings, stretch tuning, tuning error, voice priority; MPE (2 destinations each).
- `[ ]` A band-limited non-modelled version would be cheaper.

**Collision (713-722)** [L] {PM}
- `[ ]` Not stated. Modelled mallet percussion (AAS): exciters feeding two stereo resonators.
- `[ ]` Mallet (stiffness, noise, colour); Noise exciter with filter and ADSR.
- `[ ]` Resonators: Beam, Marimba, String, Membrane, Plate, Pipe, Tube; Quality Eco-High; decay, material/radius, ratio, brightness, inharmonicity, opening, hit position with random, Note Off damping, listen L/R; tuning with pitch env; bleed, gain, pan; copy 1↔2.
- `[ ]` 2 LFOs that can modulate each other; MIDI/MPE tab; 1>2 or 1+2 structure.

**Drift (722-730)** [M] {PM: DFM-1, MS2 filters}
- `[ ]` Not stated. Low-CPU subtractive, fully MPE.
- `[ ]` Osc 1 (7 waves incl. Shark Tooth, Saturated) with Shape mod; Osc 2 (5 waves); noise; per-source filter bypass; two gain-staged saturation points; waveform display.
- `[ ]` 12/24 dB LP, key tracking, HP, XY display; Env 1 and Env 2/Cycling (Rate, Ratio, Time, Sync; Tilt, Hold).
- `[ ]` 9-wave LFO; 3-slot mod matrix; ratio-mode LFO can do FM.
- `[ ]` Poly/Mono/Stereo/Unison up to 32 voices; per-voice Drift randomisation.

**Drum Sampler (730-735)** [M] {ML?: similar swap}
- `[ ]` Not stated. One-shot playback for Drum Rack pads.
- `[ ]` Start, length, gain; AHD with infinite hold; Trigger/Gate; transpose and detune.
- `[ ]` 9 playback effects on an XY pad: Stretch, Loop, Pitch Env, Punch, 8-Bit, FM, Ring Mod, Sub Osc, Noise.
- `[ ]` 4 filter types; velocity or slide to filter, envelope stages, FX1/FX2.
- `[ ]` Similar-sample swapping and reference; hot-swap; Envelope Follows Pitch; convert to Simpler; Save as Default Pad.
- `[ ]` Depends on Drum Rack, browser.

**Electric (736-741)** [L] {PM}
- `[ ]` Not stated. Modelled tine electric piano (AAS).
- `[ ]` Hammer (stiffness, noise, force); Fork tine and tone (colour, decay, release); Pickup (symmetry, distance, R/W type, in/out); damper noise.
- `[ ]` Global: voices, tuning, stretch, bend and per-note bend.

**External Instrument (741-742)** [M] {HW optional}
- `[ ]` Not stated. Routing utility: MIDI to port or multitimbral plug-in, audio returned.
- `[ ]` Gain; manual hardware latency in ms or samples.
- `[ ]` Depends on delay compensation, routing, plug-in host.

**Impulse (743-745)** [M]
- `[ ]` Not stated. 8-slot drum sampler.
- `[ ]` Per slot: start, transpose, Mode A/B stretch, filter, saturator, Trigger/Gate decay, pan, volume, velocity/random mods; play/solo/mute/hot-swap.
- `[ ]` Parameters latch at note-on. Slot 7/8 mutual cut.
- `[ ]` Global volume, transpose, Time; individual slot outputs.

**Meld (745-755)** [XL] {PM: some filters}
- `[ ]` Not stated. Two macro-oscillator engines, each with filter, envelopes, LFOs and matrix.
- `[ ]` Manual says 24 oscillator types but describes 25: Basic, Dual Basic, Noisy Shapes, Square Sync, Square 5th, Sub, 4 Swarms, Harmonic/Fold/Simple FM, Squelch, Chip, Shepard's Pi, Tarp, Extratone, Noise Loop, Filtered Noise, Bitgrunge, Crackle, Rain, Bubble, Chord.
- `[ ]` Two type-specific macros per oscillator; each engine has Amp and Mod envelopes (Trigger, Loop, AD Loop; Link).
- `[ ]` LFO 1 (6 types plus two serial FX slots of 18) and LFO 2 (6 shapes).
- `[ ]` 17 filters: SVF, MS2, OSR, LP Crunch, Switched Res, Filther, EQ peak/notch, Phaser, Redux, Vowel, Comb +/-, Plate and Membrane modal.
- `[ ]` Matrix with expanded view and A/B copy; MIDI and MPE tabs; key tracking off, phase reset/spread, porta/gliss.
- `[ ]` Scale awareness: 6 oscillators, 2 filters; Chord also follows the scale.
- `[ ]` Per-engine tone filter, per-voice drive and limiter; mono/poly 2-12, stacked voices, Spread.

**Operator (755-772)** [L] {PM: filter circuits}
- `[ ]` Not stated. 4-oscillator FM combined with subtractive and additive; 11 algorithms.
- `[ ]` Waves: sine, 4/8-bit, digital saw/square, resynthesised saw/square/triangle, 2 noises; harmonics editor 16/32/64 with Repeat, odd/even, Normalize, .ams export/import.
- `[ ]` Coarse/fine or fixed frequency, velocity-to-pitch with quantise, phase, retrigger, feedback.
- `[ ]` 7 envelopes (Loop, Beat, Sync, Trigger; slopes on filter and pitch); LFO reaching audio rate.
- `[ ]` Filter with waveshaper; Time, Tone, Antialias, Interpolation; 5 MIDI sources to 2 destinations; polyphonic glide, Spread; 32 voices.

**Sampler (773-791)** [XL] {SAMPLES; CONTENT: presets}
- `[ ]` Editions: REX import is Standard and Suite only; p. 774 implies not every edition includes Sampler.
- `[ ]` Multisample playback built for multi-GB libraries.
- `[ ]` Zone Editor: key, velocity, sample-select zones with lin/power fades, layer list with sort, distribute, normalize, Auto Select.
- `[ ]` Round Robin: Forward, Backward, Other, Random, reset interval.
- `[ ]` Sample tab: root, detune, volume, pan, start/end, sustain and release loops incl. back-and-forth, crossfade, loop detune, zero-cross snap, global reverse, interpolation quality, RAM mode.
- `[ ]` Modulation oscillator FM/AM (21 waves, envelope); pitch envelope, Spread, key zone shift, Glide/Portamento.
- `[ ]` Filter plus shaper with order switch; volume envelope; aux envelope (29 destinations); 3 LFOs (2 and 3 stereo Phase/Spin).
- `[ ]` 7 MIDI sources to 2 destinations.
- `[ ]` Imports REX, ACID, Soundtrack Loops to presets or Racks; convert to Simpler.

**Simpler (791-802)** [L] {PM: Complex warp modes}
- `[ ]` Not stated. Single-sample player with synth voice and clip-style warping.
- `[ ]` Classic (region, loop, snap, fade, ADSR); One-Shot (mono, Trigger/Gate, fades).
- `[ ]` Slicing: Transient, Beat, Region, Manual up to 64, Mono/Poly/Thru.
- `[ ]` Warp with Warp As, halve/double; clip drag preserves Warp Markers.
- `[ ]` Filter, 3 ADSRs, amp loop modes, per-voice LFO, Spread, glide.
- `[ ]` Non-destructive Crop and Reverse; Slice to Drum Rack or new MIDI track; convert to Sampler; expanded view.
- `[ ]` Depends on warp engine, transient detection, Drum Rack.

**Tension (802-813)** [XL] {PM}
- `[ ]` Not stated. Modelled strings (AAS).
- `[ ]` Exciters: Bow, Hammer, bouncing Hammer, Plectrum; Damper; String (decay, ratio, inharmonicity, damping).
- `[ ]` Vibrato; Termination (finger, fret); Pickup; Body (type, size, decay, mix, cuts).
- `[ ]` Filter with envelope and LFO; MPE; stretch and tuning error, voice priority; portamento; unison.

**Wavetable (814-823)** [L] {CONTENT: factory tables; PM: filter circuits}
- `[ ]` Not stated. 2 wavetable oscillators plus sub.
- `[ ]` WAV/AIFF import with automatic cleanup or Raw; linear/polar view; oscillator effects FM, Classic (PW, Sync), Modern (Warp, Fold).
- `[ ]` 2 filters, Serial/Parallel/Split.
- `[ ]` Modulation matrix and MIDI tab; 3 envelopes; 2 shapeable LFOs; global time and amount.
- `[ ]` 6 unison modes; Hi-Quality toggle (off means modulation every 32 samples and low-power filters).


## 32. Max for Live (824-829)

**Milestones:** Out of scope (Max runtime and scripting API)


**Requires:** Max runtime and editor {MAX}; Suite, or Standard with the add-on.

- `[out]` Instruments, audio and MIDI effects, MIDI Tools, Modulators; Live API access [XL] {MAX}
- `[out]` Edit in Max; update all instances or one on save; custom Max path; developer-mode option [XL] {MAX}
- `[out]` AMXD files stored separately in User Library; repaired via File Manager [M]
- `[out]` MIDI Tools are discovered via default folders or Places [M]
- `[out]` Device freeze bundles dependencies [M]

**Without Max:** AMXD compatibility isn't realistic. Parity of purpose means a native scripting or plug-in API over notes and parameters [XL]. Dependency: a stable public object model. Every device in chapter 33 can be native.


## 33. Max for Live Devices (830-864)

**Milestones:** B1, B2, B13


Listed as editable with Suite or Standard plus add-on; otherwise editions not stated. All can be native, no Max needed. No proprietary algorithm is claimed.

**Instruments (8):** each has a preview click, decay and volume.
- `[ ]` DS Clang (831) [S]: two tones, noise, filter, clave repeats
- `[ ]` DS Clap (832) [S]: noise plus impulse through panned delays; Sloppy, Tail, Spread
- `[ ]` DS Cymbal (833) [S]: sine and pulse plus high-passed noise
- `[ ]` DS FM (834) [S]: FM with feedback, modulation-type blend
- `[ ]` DS HH (835) [S]: noise and sine; white/pink; 12/24 dB HP
- `[ ]` DS Kick (836) [S]: modulated sine, drive, overtones, click, pitch env
- `[ ]` DS Snare (837) [S]: oscillator plus filtered noise
- `[ ]` DS Tom (838) [S]: impulse plus oscillators, band-pass resonances, pitch bend env

**Audio effects (4):**
- `[ ]` Align Delay (839) [S]: time, samples or distance with temperature; per-channel or linked
- `[ ]` Envelope Follower (840-842) [M]: gain, rise, fall, delay, sidechain with mix
- `[ ]` LFO (843-845) [M]: 9 waves, shape, steps, jitter, smooth, depth, offset, phase, hold, retrigger
- `[ ]` Shaper (846-848) [M]: breakpoint envelope, curves, grid, 6 presets, scope; Loop, 1-Shot, Manual

**MIDI effects (6):**
- `[ ]` Envelope MIDI (848-850) [M]: ADSR with Free, Sync, Loop, Echo; velocity, slopes
- `[ ]` Expression Control (851-852) [M]: 5 slots from 10 sources incl. Increment and Random; curves, smoothing
- `[ ]` MIDI Monitor (853-854) [S]: note, flow, MPE views; freeze
- `[ ]` MPE Control (855-860) [M]: per-source curves and smoothing; pressure to aftertouch, slide to CC1, note bend to global; Abs/Rel/Onset slide, Centered, pitch range
- `[ ]` Note Echo (861-862) [S]: synced or ms echoes with pitch, velocity, feedback, MPE feedback
- `[ ]` Shaper MIDI (862-864) [M]: note-triggered breakpoints, sustain point, velocity, loop, echo

**Shared modulator core [L]:** map any automatable parameter, up to 8 targets; Modulation (base plus bipolar/unipolar depth) or Remote Control (min/max override). Depends on the parameter graph and parameter locking in the UI.


## 34. MIDI and Key Remote Control (865-873)

**Milestones:** A15, C16


**Hardware:** any MIDI or control surface {HW for testing only}. Scripts for vendor surfaces need per-device work.

- `[ ]` Mappable targets: slots, switches, radios, continuous controls, crossfader [M]
- `[ ]` Up to 6 named control surfaces with ports, preset dump [L] {HW}
- `[ ]` Instant mappings following selected device; lock to device [L] {HW}
- `[ ]` Manual surfaces via Remote toggles on input/output ports, merged inputs, feedback output [M]
- `[ ]` MIDI indicators showing incoming traffic [S]
- `[ ]` Takeover modes None, Pick-Up, Value Scaling [S]
- `[ ]` Mapping Browser with path, editable min/max, invert, delete [M]
- `[ ]` MIDI Map Mode with overlay; note mapping, note range for chromatic slots [M]
- `[ ]` Absolute 7-bit, 14-bit, pitch bend; reversed ranges invert switches [S]
- `[ ]` Relative types Signed Bit, Signed Bit 2, Bin Offset, Twos Complement, plus linear variants; auto-detect [M]
- `[ ]` Relative Session strip: scene up/down, scroll, launch, cancel, track launches [S]
- `[ ]` Key Map Mode [S]
- `[ ]` Mapped keys stop reaching the track input [S]

Depends on a universal parameter identity and clip-slot addressing.


## 35. Push 1 (874-913) {HW}

**Milestones:** A18


**Requires:** Ableton Push 1 over USB, auto-detected, no driver or setup.

- `[ ]` Browse Mode with load-next, Drum pad and rack browsing [L]
- `[ ]` Drum layouts: Loop Selector, 16 Velocities, 64-pad; pad colours; choke via Duplicate/Select/Mute/Solo/Delete modifiers [L]
- `[ ]` Step sequencer with variable resolution and triplets; note nudge, length, velocity per step [L]
- `[ ]` Melodic sequencer; Melodic + 32 Notes; loop pages, auto-follow, page duplicate [L]
- `[ ]` In-key/chromatic grid, Fixed, 4ths/3rds/Sequent layouts, scales saved per Set [M]
- `[ ]` Record/overdub cycle; New; Fixed Length; Repeat with swing and pressure; Accent; Quantize; Double; Capture via New [M]
- `[ ]` Device, Volume, Pan & Send, Track, Clip modes; banks and Racks via In/Out [L]
- `[ ]` Session grid, Stop/Mute/Solo, Session Overview 64×64 [M]
- `[ ]` Session automation recording, per-step automation, re-enable overrides [L]
- `[ ]` Velocity curves, aftertouch threshold; Scene vs Clip Workflow; touch strip; footswitches; User Mode [M]


## 36. Push 2 (914-972) {HW}

**Milestones:** A18


**Requires:** Ableton Push 2 with its power supply, USB, firmware delivered through Live.

Adds to Push 1:
- `[ ]` Colour display UI with waveforms, parameters, track colours, clip view [XL]
- `[ ]` Pad and clip colour editing; pad choke and transpose encoders [S]
- `[ ]` Simpler on hardware: Classic, One-Shot, Slicing with Pad Slicing, Nudge, Split Slice, Legato playback [L]
- `[ ]` Convert: Simpler to Drum Rack, pad to track, audio clip to Simpler, Drum Rack or Harmony/Melody/Drums MIDI [L] {ML?}
- `[ ]` Track and Global Mix, routing from hardware; device add, delete, reorder; Rack and Group unfolding [L]
- `[ ]` Phrase Sync; Arrangement recording; count-in bar; Preview and Load Next/Previous [M]
- `[ ]` Clip Mode with folded note view, sequenceable-area indicators, Crop [L]
- `[ ]` Pad Sensitivity, Gain, Dynamics; display and LED brightness [S]

**Without Ableton's hardware (Push 1 and 2):** only the Push firmware, display and protocol are specific to it. User Mode (pp. 905, 970) releases Push for reassignment, but this range gives no protocol details. The pad layouts, step sequencing, modal mixing and overview can all be an on-screen grid or a generic 8×8 controller [L]. Build them on the core features rather than as Push code. Treat native Push and its display as optional [XL] {HW}.


## 37. Link, Tempo Follower, MIDI Sync (973-981)

**Milestones:** A16


- `[ ]` Link tempo, beat and phase over network; peers join and leave; last tempo change wins; overrides tempo automation; count-in disabled; progress bar while waiting [L] {SVC: Ableton technology; SDK licensing is outside this manual}
- `[ ]` Start Stop Sync; peer count; hideable toggle [S] {SVC}
- `[ ]` Link Audio streaming; peers as input types; name, latency, Sync to Incoming Audio for one peer [XL] {SVC}
- `[ ]` Tempo Follower from an audio input; excludes External Sync but can still send clock [L] {ML?}
- `[ ]` MIDI Clock send and receive, song position, loop wrapping [M]
- `[ ]` MIDI Timecode receive only, frame rates, SMPTE All, offset [M]
- `[ ]` Sync Delay per device [S]

Dependencies: audio clock, launch quantisation; Link and Follower drive tempo.


## 38. Computer Audio Resources (982-988)

**Milestones:** A12, A13


- `[ ]` Multi-core graph processing [XL]
- `[ ]` CPU meter: Average, Current, text choice; optional overload warning [S]
- `[ ]` Disable unused I/O channels to save load [S]
- `[ ]` Idle devices sleep, tail-aware [M]
- `[ ]` Per-track Performance Impact 6-segment meter [M]
- `[ ]` Freeze/Unfreeze clips to samples, frozen tails, edit limits while frozen, real-time freeze for external gear, Processed/Freeze folder, frozen-to-new-track drag [L]
- `[ ]` RAM Mode; Bounce in Place including groups [M]
- `[ ]` Overload indicator for Disk/CPU [S]


## 39. Audio Fact Sheet (989-996)

**Milestones:** Verification targets for all audio milestones


No Ableton hardware, runtime or service is required. This chapter is an acceptance spec; everything is achievable with own DSP except Complex-mode sound parity {PM}. Null-test automation [M].

**Neutral requirements:**
- `[ ]` Undithered render at matching rate and bit depth or higher [S]
- `[ ]` Unstretched playback, including Beats, Tones, Texture, Re-Pitch at native tempo, and tempo round-trip [M]
- `[ ]` 64-bit summing at single mix points with 32-bit processing [M]
- `[ ]` Neutral recording when bit depth is at least the converter, or 32-bit internal [S]
- `[x]` Bypass removes devices but keeps lookahead delay [S]
- `[ ]` Routing and clip split are neutral [S]
- `[ ]` Frozen files are 32-bit; Session freeze is two loop cycles and snapshots parameters; random values get baked [M]

**Non-neutral by design:**
- `[ ]` Complex modes always [L] {PM}
- `[ ]` Sample-rate conversion and transposition; export uses SoX Resampler (LGPL 2.1), available to rLive [S]
- `[ ]` Per-sample volume automation [S]
- `[ ]` Dither, once only [S]
- `[ ]` Consolidate normalises and matches project format [S]
- `[ ]` Clip fades up to 4 ms [S]
- `[ ]` Constant-power sinusoidal panning, +3 dB hard panned [S]
- `[ ]` Grooves [M]


## 40. MIDI Fact Sheet (997-1002)

**Milestones:** A11, A16


No Ableton dependency; behaviour depends on OS drivers and MIDI hardware. All achievable.

- `[ ]` Consistent latency preferred over jitter [M]
- `[ ]` Driver timestamps on input; record what is heard when monitoring by adding buffer-based delay [M]
- `[ ]` Timestamped output scheduling; internal scheduling where MME lacks timestamps [M]
- `[ ]` Keep capturing MIDI through audio dropouts [M]
- `[ ]` Published rig: split MIDI to a zero-latency MIDI-to-audio path; 36 configurations; most events ±1 ms; rare outliers to ±11 ms [M]
- `[ ]` Monitoring off advice for hardware synths [S]

Benchmark: match those jitter figures.


## 41. Accessibility and Keyboard Navigation (1003-1013)

**Milestones:** C19


- `[ ]` Screen readers through native OS accessibility on macOS and Windows [XL] (egui's AccessKit helps; custom widgets are the cost)
- `[ ]` High-contrast theme [S]
- `[ ]` Tab focus mode, wrap, neighbour navigation; Navigate menu; Move Clips with Arrows option [L]
- `[ ]` Settings keyboard navigation; language selection [M]
- `[ ]` Speak toggles: menu commands, slider limits, seconds; Speak Help Text [M]
- `[ ]` Session keyboard flows: title bars, slots, scenes, mixer [L]
- `[ ]` Arrangement flows: locators, clip edges, selections, export [L]
- `[ ]` MIDI editing by keyboard: selection, transpose, length, velocity, probability [M]
- `[ ]` Audio editing: warp markers and transients by keyboard [M]
- `[ ]` Device and Browser keyboard flows, preview, multi-select [M]
- `[ ]` Keyboard breakpoint create, jump, select, value entry, focus and undo colours [M]
- `[ ]` Switching automation lanes by keyboard [S]

Stated gaps Live itself has: tagging/filters, MPE editing, Max, audio fades, parts of complex devices. These are not required for parity.


## 42. Keyboard Shortcuts (1014-1036) [M: focus-scoped keymap; features counted elsewhere]

**Milestones:** C1


Notation: **Cmd** is Ctrl on Windows; **Alt** is Option on Mac; W:/M: marks differences.

- `[ ]` **Views:** F11 / M: Ctrl+Cmd+F fullscreen; Cmd+Shift+W second window; Tab Session/Arrangement; Shift+Tab or F12 Device/Clip; Alt-click view toggle shows both; Q hot-swap; D rack/last pad; Shift+? Info; Cmd+Alt + V video, B browser, O overview, I in/out, S sends, M mixer, 3 clip, 4 device, 5 browser, 6 groove, 7 learn; Cmd+, settings; Esc close.
- `[ ]` **Focus:** Alt+0-7 Control Bar, Session, Arrangement, Clip, Device, Browser, Groove, Learn; Alt+8 selected clip panel; Alt+Shift+P clip panels; Tab/Shift+Tab; W: Ctrl+Tab / M: Alt+Tab neighbour, +Shift previous.
- `[ ]` **Sets:** Cmd+N/O/S; Cmd+Shift+S; Cmd+Q; M: Cmd+H; Cmd+Shift+R export audio/video; Cmd+Shift+E export MIDI.
- `[ ]` **Devices:** Cmd+G group; Cmd+Shift+G ungroup; Alt-click activator for group; Shift-click add; right-click selector lists devices; Delete/double-click reset; Enter load; Cmd+Alt+P plug-in windows; Cmd on plug-in button keeps multiple; P A/B.
- `[ ]` **Edit:** Cmd+X/C/V/D; Delete; Cmd+Z; W: Ctrl+Y / M: Cmd+Shift+Z redo; Cmd+R rename; Cmd+A; Cmd-click, Shift-click; Tab next while renaming; W: Alt / M: Cmd ignores grid. Modifiers: Shift across tracks or time, Alt on envelope part.
- `[ ]` **Values:** arrows; Shift+arrows octave/fine; Shift-drag fine; Delete default; digits; . or , next field; Esc; Enter.
- `[ ]` **Envelopes:** A automation; Alt curve; F fades; Cmd+Delete delete envelope.
- `[ ]` **Loop/markers:** Cmd+F9/F10/F11/F12 start, loop start, loop end, end. When selected: Cmd-click, Cmd+Shift-click move; L/R nudge; U/D move by length; Cmd+U/D halve/double; Cmd+L/R resize; Cmd+Shift+L select.
- `[ ]` **Zoom:** Cmd +/- window; +/- ruler; Alt+Shift+F follow; Shift-scroll; Cmd+Alt pan; Shift or Cmd click extend.
- `[ ]` **Clip tabs:** W: Ctrl+Tab / M: Alt+Tab; Alt+Shift+1/2/3.
- `[ ]` **Sample editor:** Cmd+U; Cmd+Shift+U; arrows move marker; Cmd+L/R select marker; W: Ctrl+Shift+F / M: Alt+Shift+F follow; Shift+L/R region; Z/X zoom; Cmd+I marker; Cmd+Shift+I transient; Delete; Cmd+Shift+Delete transient; W/H fit.
- `[ ]` **Notes:**
  - `[ ]` Cmd+A; W: Ctrl-drag / M: Alt-drag copy; Cmd+E chop or split; Cmd+E drag ±1; Cmd+Shift+E drag ±2; hold E split; Cmd+J join; Cmd+Alt+J fit; Cmd+U.
  - `[ ]` PgUp/Dn vertical, Shift fine, Cmd horizontal; W: Alt-drag / M: Cmd-drag velocity; Home/End; Alt+U/D next note; Alt+L/R same key.
  - `[ ]` Digits+Enter velocity; Cmd+U/D velocity; Cmd+Shift+U/D deviation; Cmd+Alt+U/D chance; Cmd+Alt+E full size; Cmd+G/Cmd+Shift+G play-all groups; Cmd+Enter apply tool; Cmd+Shift+A invert; K scale; Cmd+Shift+F note filters.
- `[ ]` **Grid:** B draw; Cmd+1 narrow, 2 widen, 3 triplet, 4 snap, 5 fixed/adaptive. **Global quantise:** Cmd+6 16th, 7 8th, 8 quarter, 9 bar, 0 off.
- `[ ]` **Session:** Enter launch; arrows; W: Ctrl-drag / M: Alt-drag copy; Cmd+E stop button; Cmd+Enter stop track; Cmd+Shift+M MIDI clip; Cmd+I scene; Cmd+Shift+I captured; PgUp/Dn 8 scenes; Cmd+Shift+F9 record; Shift+Enter follow actions; Cmd+Shift+Enter chain; Cmd+L/R move track; Cmd+U/D move scenes; Cmd on drop makes scene; 0 deactivate; Esc title; Home/End; S solo chain.
- `[ ]` **Arrangement:**
  - `[ ]` Cmd+E split; Cmd+J consolidate; Cmd+Shift+J crop; Enter+L/R resize; Shift+Alt-drag slide; Shift-drag title stretch; Shift+Alt title-drag select.
  - `[ ]` Cmd+Alt+F fades; Cmd+Alt+Backspace delete fades; Cmd+L loop; Cmd+I silence; Cmd+Shift+X/C/V/D/Delete time.
  - `[ ]` U fold; Alt+U unfold; Alt+/- height; H/W optimise; R reverse; W: Ctrl+Space / M: Alt+Space play from insert; Cmd+Shift+Space insert to playhead; Alt+Shift+M mixer.
- `[ ]` **Comping:** Cmd+Alt+U lanes; Enter commit; T audition; Shift+Alt+T add; Cmd+D; Cmd+U/D swap take. **Bounce:** Cmd+B; Cmd+Alt+V paste bounced.
- `[ ]` **Tracks:** Cmd+T audio; Cmd+Shift+T MIDI; Cmd+Alt+T return; +/- groups; U collapse; Cmd+Alt+R returns; Cmd+arrows move; C arm; S solo; 0 deactivate; Cmd+Alt+Shift+F freeze; Delete from title.
- `[ ]` **Transport:** Space; Shift+Space continue; W: Ctrl+Space / M: Alt+Space stop at selection end; Home; F9 record; Shift+F9 arm Arrangement; F10 back to Arrangement; F1-F8 activators; O metronome; Cmd+Alt+Shift+E engine.
- `[ ]` **Browser:** arrows; Enter; Shift+Enter or right preview; Cmd+F; Down/Enter results; 1-7 colour, 0 reset; Cmd+Shift+F similar {ML?}; Cmd+[ and ]; Cmd+Alt+G filter; Cmd+Shift+E tags. **Similar swap:** Cmd+R/L next/previous, Up save, Down return; Alt shows controls.
- `[ ]` **Mapping:** Cmd+M MIDI; Cmd+K key; M computer keyboard; Z/X octave; C/V velocity; Shift+letter while active.
- `[ ]` **Latching:** holding ~500 ms on A, B, S, Z, F1-F8, Tab; Options.txt can disable. **Menus:** W: Alt+letter; M: Cmd+?; Menu or Shift+F10 / VO+Shift+M.

Manual inconsistencies to resolve:
- `[x]` Export is Cmd+Shift+R on p. 1016 but Cmd+R on p. 1008.
- `[x]` Note next/previous is Alt on p. 1023 but Ctrl (Win) on p. 1009.

Requirement: Cmd+Shift+F, Cmd+Shift+E, Cmd+I, Cmd+E and Z/X all change meaning by focus, so the keymap must be context-scoped.

Shortcuts declared in the keymap that do nothing yet, by milestone. This list is generated from `crates/rlive-app/src/ui/keymap/bindings.rs`.

<!-- pending-shortcuts:begin -->
- `[ ]` **A11:** Arm recording in Arrangement; Record to Session View
- `[ ]` **A12:** Bounce to new track; Freeze or unfreeze tracks; Paste bounced audio
- `[ ]` **A14:** Focus Groove Pool; Hide/show Groove Pool
- `[ ]` **A17:** Hide/show plug-in windows; Keep several plug-in windows open
- `[ ]` **A19:** Next similar sample; Previous similar sample; Return to reference; Save as similarity reference; Show similar-sample swap controls; Similarity search
- `[ ]` **A5:** Audio engine on/off; Hide/show In/Out
- `[ ]` **A9:** Delete Warp Marker; Delete transient; Insert Warp Marker; Insert transient; Move Warp Marker left; Move Warp Marker right; Quantize; Quantize settings; Reverse audio selection; Select next Warp Marker; Select previous Warp Marker; Stretch warped clip
- `[ ]` **C10:** MPE tab
- `[ ]` **C11:** Apply MIDI Tool settings
- `[ ]` **C12:** Add device from browser; Assign colour 1; Assign colour 2; Assign colour 3; Assign colour 4; Assign colour 5; Assign colour 6; Assign colour 7; Browser history back; Browser history forward; Close folder; Drop browser clips as a scene; Hide/show Filter View; Hide/show Tag Editor; Hot-swap selected device; Jump to search results; Load selected device; Load selected item; Open folder; Preview selected file; Reset assigned colour; Scroll down; Scroll up; Search in Browser; Toggle Hot-Swap Mode
- `[ ]` **C14:** Export MIDI File
- `[ ]` **C15:** Add device to selection; All devices in a group on/off; Compare A/B; Group devices; List a track's devices; Solo selected chain; Toggle Drum Rack/last selected pad; Ungroup devices
- `[ ]` **C16:** Toggle Key Map Mode; Toggle MIDI Map Mode
- `[ ]` **C17:** Hide/show Sends; Hide/show return tracks; Toggle second window
- `[ ]` **C18:** Hide/show video window
- `[ ]` **C19:** Cancel value entry; Confirm value entry; Current scene from the last track; Decrement value; Delete track from title bar; Eight scenes down; Eight scenes up; First track of the scene; Focus Mixer; Increment value; Jump to highlighted track title; Last track of the scene; Move nonadjacent tracks down; Move nonadjacent tracks left; Move nonadjacent tracks right; Move nonadjacent tracks up; Next control; Next field; Next name while renaming; Next neighbouring control; Next track while renaming; Previous control; Previous neighbouring control; Return to default; Scene down; Scene up; Select all clips and slots; Select clip above; Select clip below; Select clip left; Select clip right; Type a value; Value down by an octave or fine step; Value up by an octave or fine step
- `[ ]` **C2:** Double loop length; Envelopes tab; Fit content to view height; Fit content to view width; Fixed or zoom-adaptive grid; Focus clip panels; Focus selected clip panel; Follow playback; Halve loop length; Hide/show Overview; Lengthen loop; Lengthen loop brace; Move clip region left with start marker; Move clip region right with start marker; Move end marker to position; Move loop by its length, down; Move loop by its length, up; Move start marker to position; Narrow grid; Nudge loop brace left; Nudge loop brace right; Sample or Notes tab; Select loop brace contents; Select material in loop; Set end marker; Set loop end; Set loop start; Set start marker; Shorten loop; Shorten loop brace; Show Device and Clip View together; Snap to grid; Switch Notes/Envelopes/MPE tabs; Switch Sample/Envelopes tabs; Toggle full-size Clip View; Triplet grid; Widen grid; Zoom back from clip selection; Zoom to clip selection
- `[ ]` **C3:** Curve an automation segment; Delete the selected envelope; Edit the selected part of an envelope; Finer envelope resolution; Ignore grid while dragging breakpoints; Toggle Automation Mode
- `[ ]` **C4:** Consolidate selection; Copy time; Crop selected clips; Cut time; Deactivate selection; Delete time; Duplicate time; Edit clips and slots across all tracks; Edit time across all tracks; Fold or unfold selected tracks; Fold selected tracks; Insert marker to playhead; Insert silence; Nudge selection left; Nudge selection right; Optimize height; Optimize width; Pan beside a selection; Paste time; Play from insert marker in clip; Play the selection; Resize clip at the insert marker; Select time within a clip; Shorter tracks or clips; Slide waveform; Split Arrangement clip at time selection; Split clip at selection; Stop at the end of the selection; Taller tracks or clips; Unfold all tracks; Unfold selected tracks; Zoom back from time selection; Zoom to time selection
- `[ ]` **C5:** Create fade or crossfade; Delete fades in selected clips; Toggle fade controls for a moment; Toggle fade handles for a moment
- `[ ]` **C6:** Add take area to the main lane; Add take lane; Audition take lane; Duplicate take lane; Next take into the main clip; Previous take into the main clip; Show take lanes
- `[ ]` **C7:** Add or remove stop button; Deactivate selected clip; Insert captured scene; Move nonadjacent scenes down; Move nonadjacent scenes up
- `[ ]` **C8:** Create Follow Action chain; Toggle Follow Actions
- `[ ]` **C9:** Change velocity from the note editor; Chop notes or split at time selection; Chop notes, one at a time; Chop notes, two at a time; Copy notes; Fit notes to time range; Fit notes to view height; Fit notes to view width; Group notes (play all); Highlight scale; Insert marker to beginning; Insert marker to end; Invert note selection; Join notes; Lower note chance; Lower note velocity; Lower velocity deviation; Quantize; Quantize settings; Raise note chance; Raise note velocity; Raise velocity deviation; Scroll editor down; Scroll editor down, fine; Scroll editor left; Scroll editor right; Scroll editor up; Scroll editor up, fine; Select next note; Select next note in key track; Select previous note; Select previous note in key track; Set note velocity by typing; Show/hide note filters; Split notes at an exact place; Toggle Draw Mode; Ungroup notes; Zoom back from clip selection; Zoom in horizontally; Zoom out horizontally; Zoom to clip selection
- `[ ]` **out:** Focus Learn View; Hide/show Learn View
<!-- pending-shortcuts:end -->


## 43. Credits (1037-1039)

**Milestones:** None


No features. AAS is credited for modelling, which confirms the {PM} tags. Listed sound content partners back {CONTENT}/{SAMPLES}. REX2, VST and ASIO appear as third-party trademarks; check their terms before implementing.


## Key dependency chains

- Parameter identity → automation, mapping, modulators, Push modes, accessibility.
- Warp engine and transient detection → Simpler, Drum Sampler stretch, Convert, Freeze playback, neutrality tests.
- Rack model → Drum Sampler, slicing to Drum Rack, Impulse-like outs, Push rack navigation.
- Filter, envelope, LFO and voice crates → all synths; do these before any instrument.
- Audio/MIDI clock with timestamps → sync, recording accuracy, launch quantisation.
- Browser and analysis index → hot-swap, similarity, MIDI Tools discovery.


## Ten features that most define Live in this range

1. Simpler warping and slicing to Drum Rack
2. Grid-based note, step and clip workflow (Push model, hardware-independent)
3. Universal MIDI and Key mapping with instant mappings
4. Modulators that can map any parameter
5. Operator
6. Wavetable
7. Sampler multisampling and deep modulation
8. MPE across the instruments
9. Link synchronisation
10. Freeze with neutral, documented audio behaviour

**Count:** 13 instruments in chapter 31 (12 sound generators plus the External Instrument routing utility), and 18 Max for Live devices (8 instruments, 4 audio effects, 6 MIDI effects).
