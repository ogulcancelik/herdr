# libghostty-vt local patches

This file tracks intentional local changes applied on top of the vendored
`libghostty-vt` source. Remove a patch only when the vendored source commit
contains the upstream fix and the listed verification still passes.

## 0001 backport resizeCols cursor subtraction saturation

status: active

patch: `vendor/patches/libghostty-vt/0001-backport-resizecols-cursor-subtraction.patch`

herdr issue: https://github.com/ogulcancelik/herdr/issues/465

upstream discussion: https://github.com/ghostty-org/ghostty/discussions/12905

upstream pr: https://github.com/ghostty-org/ghostty/pull/12907

introduced upstream: `c44afa625`

vendored base: `0f7cd84b880b203c98683e520e84b9db0c5938d8`

local files:

- `vendor/libghostty-vt/src/terminal/PageList.zig`
- `vendor/libghostty-vt/src/terminal/c/terminal.zig`

reason: shrinking rows and columns in one resize can leave the pre-resize
cursor row past the new row count. `PageList.resizeCols` then computed rows
below the cursor with checked unsigned subtraction and aborted in safety builds.

remove when: the vendored source commit contains upstream PR #12907 and the
local ReleaseSafe resize regression tests pass without this patch.

verification:

```sh
zig build test-lib-vt -Demit-lib-vt -Doptimize=ReleaseSafe -Dtest-filter="resize shrinks both axes with cursor at bottom"
zig build test-lib-vt -Demit-lib-vt -Doptimize=ReleaseSafe -Dtest-filter="PageList resize less rows and cols cursor at bottom"
```

## 0002 expose kitty image transmit time in the C API

status: active

patch: `vendor/patches/libghostty-vt/0002-expose-kitty-image-transmit-time-ns.patch`

herdr issue: https://github.com/ogulcancelik/herdr/issues/947

upstream discussion: https://github.com/ghostty-org/ghostty/discussions/13177
(proposes extending the kitty graphics inspection C API from
https://github.com/ghostty-org/ghostty/pull/12145, which has no transmit
time/serial accessor yet)

introduced upstream: not yet

vendored base: `0f7cd84b880b203c98683e520e84b9db0c5938d8`

local files:

- `vendor/libghostty-vt/include/ghostty/vt/kitty_graphics.h`
- `vendor/libghostty-vt/src/terminal/c/kitty_graphics.zig`

reason: herdr fingerprints kitty image data to decide when to re-encode an
image for render clients. Hashing the full payload on every render is too
expensive for multi-megabyte images, and sampling windows misses small
changes, freezing streaming sources. The image's transmit time already
refreshes on every (re)transmission, so exposing it as
`GHOSTTY_KITTY_IMAGE_DATA_TRANSMIT_TIME_NS` gives herdr an exact, O(1) change
serial to invalidate a cached full-data fingerprint.

remove when: the vendored source commit exposes the image transmit time (or an
equivalent transmission serial) in the C API and
`ghostty::tests::kitty_image_fingerprint_refreshes_on_retransmission` passes
without this patch.

verification:

```sh
zig build test-lib-vt -Dtest-filter="image_get transmit_time_ns changes on retransmission"
cargo nextest run kitty_image_fingerprint
```
