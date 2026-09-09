# Review remediation (from the 2026-09-08 codebase review)

> Executes the phased plan in `docs/superpowers/audits/2026-09-08-kuvatin-review.html`
> (66 findings at v2.6.0). Each phase is a self-contained, releasable step;
> finding ids (C1, H1, M9 …) refer to that report.

## §0 Comply & slim — C1, H11

- **Licenses in the MSI (C1).** `bundle-gstreamer.ps1` now copies upstream's
  `share\licenses\` (82 component folders — every LGPL/GPL/BSD text and
  copyright notice, exactly as GStreamer ships them) into the staging dir, so
  `heat` harvests them into `[APPLICATIONFOLDER]\licenses\`. The script also
  generates `THIRD-PARTY-NOTICES.txt` (what is bundled, the license families,
  and the corresponding-source URLs: GStreamer 1.26.11 modules at
  gstreamer.freedesktop.org/src, Cerbero tag 1.26.11 for every third-party
  library, plus an on-request offer via the issue tracker). The source tree
  gains `THIRD-PARTY-NOTICES.md` and a README section; `deny.toml` +
  `cargo deny check licenses` police the Rust side.
- **Trimmed bundle (H11).** Instead of every DLL, the script stages an
  **allow-list of plugins** and computes the **flat-DLL closure by walking PE
  import tables** (regular + delay-load; pure PowerShell, no dumpbin) from
  `kuvatin.exe` and the staged plugins, keeping only DLLs that exist in
  `bin\`. `-AllPlugins` restores the old behaviour.
  - The allow-list = the union of (a) every plugin actually loaded — captured
    with `GST_DEBUG=GST_PLUGIN_LOADING:4` and a private registry across the
    whole video test suite, the exe's sequence render, and discovery +
    playback of mp4/m4v/mov/mkv/webm/avi/wmv/hevc, mp3/wav/flac, png/jpg/bmp
    — and (b) elements GES/playbin request lazily or on other hardware
    (`gstx264` for non-NVIDIA machines, `gstwasapi`/`gstdirectsound`
    fallbacks, `gstogg`/`gstflv`/`gstdav1d`, tag readers).
  - Validation: with **only the staged directory on PATH** (no SDK), a private
    registry and `GST_PLUGIN_PATH` at the staged plugins, the video suite
    passes 26/26, the exe renders a sequence to MP4, and discovery succeeds
    for every fixture except TIFF — which fails identically with the full
    runtime (pre-existing; TIFF isn't a video-mode input anyway).
  - Result: 391 DLLs / ~270 MB staged → **125 DLLs / 60 MB** (69 plugins +
    56 libraries + licenses); the MSI shrinks from **106.8 MB to 32.8 MB**
    (verified by administrative extract: 82 license folders, 185 files,
    notices, 69 plugins, 56 libraries). Dropped: x265, a52dec, dts, amr, siren,
    openh264, SvtAv1, libsrt, libstdc++, gstpython, aws/elevenlabs/ndi/
    decklink/webrtc, …
  - CI builds the release exe before staging (the closure needs it).

## §1 Data safety — H1, H2, H7, M1–M4, L1, L3
## §2 Engine guards — H3, H4, M7–M10, M14, L14, L15
## §3 Explorer & CLI — H8–H10, M12, M13, M15–M17, M34, L7–L10
## §4 Editor UX — H5, H6, M11, M18–M26, L11
## §5 Ship pipeline — H12, H13, M28–M33, L16, L17
## §6 Structure — M5, M6, M27, M35, L2, L4–L6, L12, L13

(Phases §1–§6 are filled in as they are executed.)
