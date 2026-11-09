//! Finds what a function body does without running it: whether it can fail (it contains
//! `fail` or `?`) and whether it's a generator (it contains `yield`). Nested functions and
//! arrow functions are their own bodies, so they aren't searched.
