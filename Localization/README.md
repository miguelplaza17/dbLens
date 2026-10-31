# Localization

English is the source language: every user-facing string in `Sources/` is an English literal.

- SwiftUI literals (`Text("Refresh")`, `Button("Cancel")`, `.help("…")`) are looked up as `LocalizedStringKey`s.
- Strings built as `String` use `String(localized: "…")`, also looked up in the main bundle.
- A key is the exact English text. Interpolations become format specifiers in the key:
  a `String` is `%@`, an `Int` is `%lld` (for example `"Rows %@–%@ of %@"`).
- Data is never localized: table, column and connection names, SQL, Postgres messages, command tags.

`pt-BR.json` is a flat `{ "English key": "Brazilian Portuguese text" }` table. Where word order
differs, use positional specifiers in the value (`%2$@ … %1$@`).

`tools/strings.sh` compiles the table into the app bundle's `pt-BR.lproj/Localizable.strings`.
To add a language, add `Localization/<locale>.json` with the same keys.
