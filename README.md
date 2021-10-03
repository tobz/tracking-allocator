# tracking-allocator

A [`GlobalAlloc`](https://doc.rust-lang.org/stable/std/alloc/trait.GlobalAlloc.html)-compatible allocator implementation that provides the ability to track allocation events.

## examples

As allocators are specialized bits of code, we've included an example in the `examples/` folder to
show how to use `tracking_allocator`, rather than putting abbreviated snippets in the README.  It is
extensively documented, and explains the finer points of using this crate, and what can be acheived
with it.

The actual Rust-level documentation is present, and should hopefully be clear and concise, but the
example is meant to be how you learn to use the crate, with the Rust-level documentation as a
rote "what's that type signature again?" style of reference.

When running the example, you should end up seeing output similar to this:

```
allocation -> addr=0x55e882b744f0 size=80 group_id=Some(0) tags=None
deallocation -> addr=0x55e882b74490
allocation -> addr=0x55e882b74550 size=12 group_id=Some(1) tags=None
allocation -> addr=0x55e882b74570 size=96 group_id=Some(1) tags=None
deallocation -> addr=0x55e882b74550
deallocation -> addr=0x55e882b74570
```
