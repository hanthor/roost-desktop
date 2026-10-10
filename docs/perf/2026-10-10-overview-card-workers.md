# Overview card workers (2026-10-10)

The compositor previously generated a missing workspace background card inside
`Wallpaper::card_element`. Even with settled-size caching, its crop, resize,
rounded mask and shadow blur ran synchronously on the input/render thread the
first time a wallpaper or layout was shown.

The card renderer now runs on bounded background workers. The compositor polls
completed results without waiting, uploads each finished card once, and scales
that buffer during the overview transition. Workers share immutable decoded
wallpaper pixels rather than copying a full output for every request. Identical
requests share a worker, and still-running jobs remain counted when settings
change, so rapid wallpaper/layout changes cannot create unbounded threads.

The desktop also prepares the normal two-workspace picker after wallpaper
loading. It schedules both active and neighbour sizes, including the different
rounded physical sizes at fractional output scales. Larger workspace strips and
app-grid cards are generated asynchronously on demand. A card not ready yet is
absent until its worker completes; cached pixels from a different wallpaper are
never substituted. The cache key now includes the work-area crop top, which was
previously omitted.

Unit checks cover worker bounds with stalled receivers, request deduplication,
cache reuse, crop invalidation, invalid size rejection, native/fractional-scale
preparation, and exact pixel equality with the existing synchronous card
renderer. The rounding and Gaussian shadow algorithms are unchanged.

## Qualification still required

This change removes software card rendering from the compositor event loop; it
does not eliminate that CPU work. Preparation moves the normal picker's work to
wallpaper loading and retains those card buffers. A paired GNOME 51 run on the
same hardware and image family must still measure first-open latency, transition
latency, compositor/shell CPU, GPU work, memory and total power. Test immediate
overview activation while wallpaper loading and a changed output/layout as well
as the prepared case. No performance victory follows from unit tests or from
moving computation to another thread.

The native output path polls the card results in `backdrop` before comparing
frame signatures. A new card buffer introduces a new element ID in the background
pass, so it triggers paint even when no client committed and the previous scene
was idle. The existing 16 ms dispatch timeout provides the next poll; an output
waiting for a page flip collects it once scanout is available. A regression test
covers this signature transition and the return to no repaint once stable.

The common 1280×800 picker retains roughly 4.3 MB of card pixels across the
active and neighbour cache entries, before any GPU texture storage. Preparation
also spends the same crop/resize/blur CPU work earlier in startup; it is not a
CPU-saving claim. Both the cache and the worker queue are limited to four
entries, including workers for a superseded wallpaper or layout.

Run the host-only timing diagnostic with:

```sh
cargo test -p tuna-compositor --lib \
  wallpaper::tests::measure_cold_card_thread_occupancy -- --ignored --nocapture
```

It reports event-thread dispatch separately from actual card-pixel readiness.
Pixel readiness excludes polling, memory-buffer upload, GPU submission and
presentation; none of these host times is an end-to-end overview measurement.

## Local diagnostic result

On the development host, with other build work active, the 922×553 cold card
check reported 188.075 ms in the original synchronous event-thread call,
0.179 ms in asynchronous dispatch, and 190.029 ms until the worker's identical
pixels were ready. The card occupied 2,269,440 bytes. This one sample verifies
where the delay occurs; it does not demonstrate lower total CPU, faster pixel
production, end-to-end overview presentation, or a GNOME comparison.

The nested compositor library compiled with `--no-default-features`; all 276
active library tests passed, including all 21 wallpaper tests (the timing check
is ignored in the ordinary suite and passed when invoked explicitly). The native
repaint module was also compiled directly against the cached Smithay dependency
and all 12 tests passed. The complete default-feature build and native VM proof
remain part of CI qualification; these checks do not exercise DRM scanout.
