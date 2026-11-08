# Goldens

Byte-for-byte expectations written by unmodified solstone-tmux 2.0.28
(`d5f92cbde08cd554f94d0841866f047714496f06`, crate `native/solstone-tmux`,
<https://github.com/solpbc/solstone-tmux>). The tests in `tests/golden_lock/`
compare the engine and the legacy reader against them.

- `engine/populated/`: one finished segment (screen JSONL, activity JSONL, zone
  file) from one poll with pane title, working directory and executable, in
  America/Denver (`tests/data/america-denver.tzif`).
- `engine/availability/`: the same poll with the executable lookup unavailable,
  in UTC.
- `legacy/pre-recovery/`: a 2.0.28 data root, written by hand: finished
  segments, one without a zone file, an interrupted segment, and `.failed` and
  `.1.failed` segments, over two stream folders. This is input, not output.
- `legacy/recovered/`: that root after 2.0.28's own start-up recovery. The lock
  file holds a run identity and is compared by presence only.

## Producing them

`generate_extraction_goldens.rs.in` drives 2.0.28's `SegmentManager` with
`TmuxAdapter::with_context(FixtureRunner, FakeLookup)` for the engine goldens,
as 2.0.28's `tests/cases/observation_transaction.rs` does, and runs
`recover_capture_streams` on a copy of `legacy/pre-recovery/`. From the
repository root:

```sh
git clone https://github.com/solpbc/solstone-tmux <scratch>/solstone-tmux
git -C <scratch>/solstone-tmux checkout d5f92cbde08cd554f94d0841866f047714496f06
cp crates/solstone-terminal/tests/data/goldens/generate_extraction_goldens.rs.in \
  <scratch>/solstone-tmux/native/solstone-tmux/tests/generate_extraction_goldens.rs
GOLDEN_OUT=<scratch>/goldens \
GOLDEN_LEGACY_PRE="$PWD/crates/solstone-terminal/tests/data/goldens/legacy/pre-recovery" \
  cargo test --locked -p solstone-tmux --test generate_extraction_goldens \
  --manifest-path <scratch>/solstone-tmux/Cargo.toml
diff -r -x .solstone-tmux.lock <scratch>/goldens/engine \
  crates/solstone-terminal/tests/data/goldens/engine
diff -r -x .solstone-tmux.lock <scratch>/goldens/legacy/recovered \
  crates/solstone-terminal/tests/data/goldens/legacy/recovered
```

`<scratch>` is any empty directory outside this repository. Both `diff`
commands print nothing when the goldens are current.
