# `cobb`

~~[Cobb](https://monkeyisland.fandom.com/wiki/Cobb) is a minor character in the classic adventure game LOOM, who also shows up in the Secret of Monkey Island wearing an "ASK ME ABOUT LOOM" button...~~ (Ah, sorry, the wrong one...)

Cobb is Rust tool that helps you track down bugs in concurrent algorithms.

Cobb is similar to [`loom`](https://crates.io/crates/loom), and was even inspired by it. It hopes to work well in a few cases where `loom` is currently lacking (too many threads, too slow, unsupported operations, ...), but in general is a much more naive approach. You generally have to either run cobb in conjunction with thread sanitizer, miri's race detector, or on weakly ordered hardware to catch many kinds of issues.

That said, if your code is sufficiently buggy, it will catch issues even on x86 without race detectors.

Cobb is very much a work in progress, and is pretty messy, but I'm getting it up now so I can link to it elsewhere.

## Usage

Cobb's attempts are more successful on weakly ordered hardware like arm. If you don't have an arm machine and can't get one, then try and make sure your code is broken on x86 as well, otherwize Cobb won't work.

See the examples directory for, well, some examples.

## Comparison to LOOM

Cobb was written due to some frustrations around using loom in practice, and while the API differs a great deal, they have almost the exact same use-case.

### Upsides compared to loom:

1. Cobb is small, simple, and easily understood... In theory anyway, the code is a bit of a mess at the moment, but it's only a few hundred lines so isn't *that* bad.
2. Cobb is much lower overhead than loom. Cobb can run more complex tests that utilize far more threads.
3. Much more accurate model of hardware reordering, as we literally just let your hardware do the reordering.
4. In loom atomic ops come with implicit SeqCst barriers, which means that a lot of bugs can't be found.

### Downsides compared to loom:
1. Has way more dependencies on your runtime environment. E.g best on weakly ordered machines with fewer threads than cores, or with external race detectors (such as thread sanitizer, or miri's new shininess).
2. If run without a race detector, it's up to you to detect that something actually went wrong (mostly).
3. Loom contains things like replacements for lazy_static that are reinitialized before runs.
4. Loom's API is much nicer, currently.

### Sidesides compared to loom:
- On loom you have a lot more control on when/where you spawn threads. Cobb just takes your funciton, and calls it a bunch from different threads.
