# Changelog

All notable changes to Subomi are documented in this file.

## [3.0.0](https://github.com/eyalmazuz/Subomi/compare/v2.0.2...v3.0.0) (2026-11-26)


### Features

* **anki,ocr,ux:** async Anki duplicate/tag badges, unified async OCR/search panel, and reaped xdg-open ([f4ac7e8](https://github.com/eyalmazuz/Subomi/commit/f4ac7e8072d1641b53dd708fa269a424d0f8386e))
* **asr:** add multi-backend -DASR_BACKENDS flag and generic ONNX Runtime ASR backend ([c246645](https://github.com/eyalmazuz/Subomi/commit/c2466452259cdc954f8dd6480f10872037a67175))
* **asr:** add seek reprioritization, in-memory PCM, safe callbacks, and Whisper/FFT optimizations ([6a69cf6](https://github.com/eyalmazuz/Subomi/commit/6a69cf65f9ae4b2c9fd91482989a71f62d0e437b))
* **asr:** implement in-memory libmpv subtitles and Whisper ASR on v2.0.2 ([77e997b](https://github.com/eyalmazuz/Subomi/commit/77e997bdf2390e6275cf4f0aa275fc2cf05e9026))
* **ass:** integrate native mpv/libass ASS rendering, interactive overlay, and live visual parity harness ([230e770](https://github.com/eyalmazuz/Subomi/commit/230e7708446bec94dea7f8b79f07a7a25f503cb9))
* **build:** make patched libmpv opt-in with -DMEMENTO_ASR_MPV and guard ASR build ([63eeae4](https://github.com/eyalmazuz/Subomi/commit/63eeae460d230fe6f34bc488dfb6dbc608b77a0b))
* **build:** provide prebuilt libmpv SDK via FetchContent with in-memory subtitle support ([96c0edb](https://github.com/eyalmazuz/Subomi/commit/96c0edb9fdbea42e35b9d63b71c7cea95494b4f2))
* **dict,ui:** wire search/ipadic-matcher setting into DictController::set_mecab_enabled ([fb2fe78](https://github.com/eyalmazuz/Subomi/commit/fb2fe7848a73e4d76311afac5e38e95e7529c899))
* **dict:** bridge MeCab morphological lookup tokens and compound DAG into DictController ([f7c34bf](https://github.com/eyalmazuz/Subomi/commit/f7c34bf5057c4296d7d77052e869c4c4c8ff6657))
* **furigana:** persistent in-process libmecab, UniDic/IPADic reading accuracy, stem alignment, CJK Ext B-I & cache ([6bd86bf](https://github.com/eyalmazuz/Subomi/commit/6bd86bfaaffad9899a365d930824361adead4118))
* improve subtitles dictionary and Anki integration ([be35b9b](https://github.com/eyalmazuz/Subomi/commit/be35b9b4e06a66ad464bddd7d98ad29e5c2a195e))
* **integration:** wire Wave-1 cross-track hooks (Anki update, auto-play-audio, rich matcher sync, platform renderers/fullscreen) ([4286a60](https://github.com/eyalmazuz/Subomi/commit/4286a6043c01ca958f6fcc1920cef81c15d59695))
* **memento-ass:** add libass layout/hitbox engine, highlight patch renderer, and MKV EBML extractor ([dc6b953](https://github.com/eyalmazuz/Subomi/commit/dc6b95381361e23abdf3fd67a63300b7f4278da5))
* **popup:** HiDPI supersampled glossaries, floating NestedYomitanPopup & recursive search fixes, and exact kana highlight length ([a761a52](https://github.com/eyalmazuz/Subomi/commit/a761a52c7d8c1241b50580c4ed95108cd689b3ec))
* **renderers:** unified platform renderers, macOS Metal bridge, Windows +1px borderless fullscreen, and Linux GL/hwdec hardening ([e120e15](https://github.com/eyalmazuz/Subomi/commit/e120e15867a06452685be9337d2ef4f2603556b3))
* restructure repository to pure Rust architecture ([969b378](https://github.com/eyalmazuz/Subomi/commit/969b378282b32b4d006983853759431fe0f1712c))
* **settings,subtitles,ui:** wire subtitle timestamps toggle and all remaining upstream settings ([90b0e1e](https://github.com/eyalmazuz/Subomi/commit/90b0e1e0e710f955fd1e8ba4b41ba913624d236a))
* **settings:** structured font/color/keybind pickers, async dict/ASR UI, and Anki note presets ([61fefb8](https://github.com/eyalmazuz/Subomi/commit/61fefb8707968a7cf96bdaccb008727d0debedaf))
* **ui,definition:** wire rich structured definitions, scoped CSS, and vector-stroked cluster subtitles ([64dce10](https://github.com/eyalmazuz/Subomi/commit/64dce1026a507d981f7fc1869d1aa2387623dac2))
* **ui,popup,subtitles:** custom popup CSS, 2-col grid, fast swash glossary, and UI/seek fixes ([410d03d](https://github.com/eyalmazuz/Subomi/commit/410d03d2606c908328b65f9f8b54352bb927bfdd))
* **ui:** add keyboard subtitle scanning, fix popup Ctrl+Down scroll and focus highlight ([566a82d](https://github.com/eyalmazuz/Subomi/commit/566a82d4b1148a2e2257b13895d73cfec2704452))
* **whisper:** upgrade to whisper.cpp v1.9.4, add CUDA sm_75 support, suppress regex option, and fix runtime warnings ([4c7fc68](https://github.com/eyalmazuz/Subomi/commit/4c7fc6862a7d1e774f1e712d82f9b7f1b6fef5e7))
* **window-ux:** wire root FocusScope keybinds, popup clamping, portal dialogs, screensaver, thumbnails, and drawer tabs/search ([b93d095](https://github.com/eyalmazuz/Subomi/commit/b93d09521a86826adfe5cdba579ff961a742b73d))


### Bug Fixes

* **asr-whisper:** canonicalize cached whisper source symlink and prioritize main repo target ([25ad7f0](https://github.com/eyalmazuz/Subomi/commit/25ad7f0ecd4a41bc1e013290124227c7176efd59))
* **asr:** observe suppress-regex reconfigure and expand controller retry tests ([eead874](https://github.com/eyalmazuz/Subomi/commit/eead874d7811e6c0032f19516ce5a3138d4c36a8))
* **ci:** fix Windows mpv.lib generation, macOS keg-only pkg-config, and Linux libmpv2 runners ([3e2eb32](https://github.com/eyalmazuz/Subomi/commit/3e2eb32b0fc02ac3694ac9d27a0297a4d87c060d))
* **ci:** fix Windows MSVC FFI/stack, macOS LC_RPATH/CJK fonts, and Linux CI tests ([e7fd04b](https://github.com/eyalmazuz/Subomi/commit/e7fd04b5d9185170bd2ed2bc0bf35a7048c830f9))
* **ci:** resolve cross-platform test assumptions and parallel cache races ([a9555c2](https://github.com/eyalmazuz/Subomi/commit/a9555c2d6e08bd6db178fb4451fca0aae512efed))
* **ci:** support mpv 0.37 screenshot-raw, IPADic/Windows tests, and Node 24 actions ([206aa8e](https://github.com/eyalmazuz/Subomi/commit/206aa8e1ff6d0c479f3780b374b1a1994e025d26))
* compact dictionary popup and improve video startup ([83d45e3](https://github.com/eyalmazuz/Subomi/commit/83d45e396efb4cf1fe8842f023d8ef46d98dcd0c))
* **dict:** pure-Rust safe Yomitan importer and SQLite engine ([ed43c6a](https://github.com/eyalmazuz/Subomi/commit/ed43c6a5f84c03b1c48f971b63ab28e1725461a5))
* **furigana:** match earliest byte offset when subtitle mixes 𠮟 and 叱 ([ead10d5](https://github.com/eyalmazuz/Subomi/commit/ead10d5f92852a3f61f8b9d4701a194f13d804ea))
* harden startup locale, XDG config, OCR/ASR FFI, HTTP client, and atomic persistence ([0ddffa0](https://github.com/eyalmazuz/Subomi/commit/0ddffa0d9c1b3a2b9c7626fa85719926ce35f27a))
* **input,fullscreen:** avoid double-click fullscreen bounce and debounce smooth-scroll seeks ([414a32d](https://github.com/eyalmazuz/Subomi/commit/414a32d199e981c7e9a61c9446ed1f760dab89bc))
* **memento-ass:** harden malformed UTF-8, EBML bounds/lacing/zlib limits, and null-renderer guards ([7dd38e0](https://github.com/eyalmazuz/Subomi/commit/7dd38e005bb6a36566c5dc0d4da0afd5a665f372))
* **p1:** resolve all Phase 1 audit findings and add regression tests ([f3f071a](https://github.com/eyalmazuz/Subomi/commit/f3f071ad19e680c84be450f75615de82055f19d6))
* **p4:** fix Slint modifier codepoints in bare_modifier_flags and refresh rich UI caches/focus ([e16f21e](https://github.com/eyalmazuz/Subomi/commit/e16f21e102841f06456841de4297be395926a796))
* **player:** resolve pause-on-end audio skip and subtitle list scroll lock during ASR ([e7dbe81](https://github.com/eyalmazuz/Subomi/commit/e7dbe81b95addf13165f57c9fc671b6241059a00))
* **popup,ass:** top-align short popup entries, keep MKV cues raw for live regex filter, and use 1-based DictResult indices ([5bbdc24](https://github.com/eyalmazuz/Subomi/commit/5bbdc2418522f13871696f5b7fb3dfeced369fc0))
* **renderers:** address verifier-v2 Win32 WS_MAXIMIZE, macOS ObjC/CF, GL alignment/sampler, and Wayland NVIDIA auto/yes findings ([7f9139f](https://github.com/eyalmazuz/Subomi/commit/7f9139f9903be5458455099728020e63d326271a))
* retain ASR sessions and repair GPU screenshot capture ([73de766](https://github.com/eyalmazuz/Subomi/commit/73de7662a49cf8bd44f920f4738a358e84809d65))
* **settings:** preserve custom font family, ignore right modifiers, label Whisper VAD, sync color picker ([394de54](https://github.com/eyalmazuz/Subomi/commit/394de54e33c0fc5cda54e3d8d4fcc3090d8b5f7e))
* **shutdown,asr,ocr:** instant clean close, guard sub-reload, finalize Python OCR ([7716c4f](https://github.com/eyalmazuz/Subomi/commit/7716c4f7e58144772f3d235d55e001532810c620))
* **subtitles,mkv:** prevent duplicate rows on upfront-extracted embedded MKV tracks and normalize CRLF/blank lines ([a56cf30](https://github.com/eyalmazuz/Subomi/commit/a56cf30523d7d52ec32c4c0dc8177aa9e400d46f))
* synchronize furigana prewarm cancel before MeCab unload and sync macOS test shortcuts ([f5d34b1](https://github.com/eyalmazuz/Subomi/commit/f5d34b10741f006738b24d4c64e6efa52af84fd9))
* **ui,asr:** shorten toast duration and stop completed ASR re-notifying on seek ([14c839e](https://github.com/eyalmazuz/Subomi/commit/14c839e6e50e93168b2eb83666c80c0f4ce9c27d))
* **ui:** resolve data_dir/res assets, custom subtitle style wrapping, focus routing, and width-keyed doc lookup ([3cddb88](https://github.com/eyalmazuz/Subomi/commit/3cddb88916606145c70c4e5d57289d159e837c0e))
* **window-ux:** wire active-cue auto-scroll, transition-gate screensaver, fix hover-pause release and secondary subtitle width ([1ffd7dc](https://github.com/eyalmazuz/Subomi/commit/1ffd7dc528ce92d9845ba88d3eaf02561779a950))
* **window-ux:** wrap sidebar panels in ancestor TouchAreas and restore root FocusScope on popup/subtitle transitions ([2316572](https://github.com/eyalmazuz/Subomi/commit/2316572cad325572f4a0b595b06a65c062565471))


### Performance Improvements

* **dict:** eliminate hot-path lookup allocations, cache regexes, and lazy-format derivations ([ac61973](https://github.com/eyalmazuz/Subomi/commit/ac619731105440fc4d216c970cf37906d08c685e))
* fast thumbnail LRU, SubEntry pre-formatted timestamps/in-place patching & re-hover lookup LRU ([80ca735](https://github.com/eyalmazuz/Subomi/commit/80ca735025e966ab14b9beefe53b4dffe83a670c))
* **net,anki,media:** enable HTTP gzip, trim/cache AnkiConnect checks, RGB screenshot pipeline ([289bb75](https://github.com/eyalmazuz/Subomi/commit/289bb7575f9da01fb5f75d716c99e0e21fa9e289))
* **ui,build:** simplify Slint components, lazy-gate closed panel models, trim regex/rfd deps, tune release profile ([ffdf00d](https://github.com/eyalmazuz/Subomi/commit/ffdf00d9346f8c1876253e24ab0591d0cb7bf404))
* **ui:** unify settings modals/overlays and simplify Slint conditionals (S1) ([d6665dd](https://github.com/eyalmazuz/Subomi/commit/d6665dd3cbd5e2b73ef631a703ae747fd7b28a33))
* **ui:** zero-copy rasterization, line-band hit-testing, Arc glossary cache, idle shrink, and async font warmup ([39f470a](https://github.com/eyalmazuz/Subomi/commit/39f470a373940894aebf55cb536cadb58c1adde0))


### Build & Packaging

* **asr:** auto-discover local ONNX Runtime and cached whisper.cpp sources ([cd11cc3](https://github.com/eyalmazuz/Subomi/commit/cd11cc33320e007ef06bbf762ec2457a9ef2a7ba))
* make ASR and OCR optional Cargo features ([552fa02](https://github.com/eyalmazuz/Subomi/commit/552fa020a0f283821c6d977461fcf45530c1d4b4))
* **ui:** isolate generated Slint UI in subomi-ui crate, trim dev DWARF, and parallelize release CGUs ([71d9849](https://github.com/eyalmazuz/Subomi/commit/71d9849f90008ee2c1fd5bba441aaaa1ee1f4598))


### Continuous Integration

* add cross-platform CI, nightly prereleases, release-please, and What's New ([74ab837](https://github.com/eyalmazuz/Subomi/commit/74ab8373f2e79cff0a7f77297e7d714cd126629a))

## [2.0.2]

### Features

* Pure-Rust & Slint rewrite of Subomi on `libmpv` and `libass`
* Instant Yomitan dictionary lookups with scoped CSS, pitch accent graphs, frequencies, and kanji footer
* Interactive subtitle drawer, per-glyph hover & keyboard subtitle scanning, and automatic MeCab furigana
* One-click AnkiConnect mining with built-in card presets (Lapis, Kiku, Senren, Animecards, JPMN)
* Optional real-time ASR (Whisper CPU/Vulkan/CUDA, ONNX Runtime) and pure-Rust Manga OCR worker
