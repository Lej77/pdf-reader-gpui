# pdf-reader-gpui

This is a GUI program that renders PDF files.

The GUI is implemented using [`gpui`](https://www.gpui.rs/): "A fast, productive UI framework for Rust from the creators of Zed".

## Usage

- Build a release version locally using `cargo build --release` then run `target/release/pdf-reader-gpui.exe`.
- Or download a precompiled executable from the [latest GitHub release](https://github.com/Lej77/pdf-reader-gpui/releases).
- When developing use: `cargo run`

## References

- [GPUI](https://www.gpui.rs/)
  - [GitHub](https://github.com/zed-industries/zed/tree/main/crates/gpui)
  - [docs.rs](https://docs.rs/gpui/latest/gpui/)
- [GPUI Component](https://longbridge.github.io/gpui-component/)
  - [GitHub](https://github.com/longbridge/gpui-component)
  - [docs.rs](https://docs.rs/gpui-component/latest/gpui_component/)

## Alternatives

There are some other Rust projects that can be used to view PDF files:
- [newinnovations/MView6: High-performance PDF and photo viewer built with Rust and GTK4](https://github.com/newinnovations/MView6?tab=readme-ov-file)
- [vincent-uden/miro: A native pdf viewer for Windows and Linux (Wayland/X11) with configurable keybindings.](https://github.com/vincent-uden/miro)
- [itsjunetime/tdf: A tui-based PDF viewer](https://github.com/itsjunetime/tdf)

## License

This project is released under [Apache License (Version 2.0)](./LICENSE-APACHE).

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
