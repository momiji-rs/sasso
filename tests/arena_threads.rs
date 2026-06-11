//! Concurrency regression test for the scoped bump arena's global region
//! registry: many threads compile under the live `ScopedAlloc` at once, so
//! each registers its own arena region and every `dealloc` classifies
//! pointers against the shared (relaxed-atomic, write-once) registry while
//! other threads are registering and bump-allocating. Each result must be
//! byte-identical to the single-threaded one.

use sasso::{compile, Options};

#[global_allocator]
static GLOBAL: sasso::ScopedAlloc = sasso::ScopedAlloc;

const SRC: &str = "\
@use \"sass:math\";
$gutter: 12px;
@mixin pad($n) { padding: $n * $gutter; }
.grid {
  width: math.div(100%, 3);
  @include pad(2);
  &:hover { color: rgba(10, 20, 30, 0.5); }
  .cell, .cap {
    margin: 1px + 2px;
    @for $i from 1 through 4 { &.s#{$i} { flex: $i; } }
  }
}
";

#[test]
fn concurrent_compiles_share_the_region_registry() {
    let expected = compile(SRC, &Options::default()).expect("single-threaded compile");
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let expected = expected.clone();
            std::thread::spawn(move || {
                for _ in 0..200 {
                    let out = compile(SRC, &Options::default()).expect("threaded compile");
                    assert_eq!(out, expected, "threaded result diverged");
                }
            })
        })
        .collect();
    for t in threads {
        t.join().expect("compile thread panicked");
    }
}
