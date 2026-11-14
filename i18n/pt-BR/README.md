# pt-BR catalog

gettext `.po` fragments, one per area of the code. The source text (msgid) is the English
string of a `tr!("…")` in Rust or a `@tr("…")` in Slint; `ui/build.rs` merges every
fragment into one catalog. The same msgid must not be translated two different ways.

The test `core::i18n::tests::catalog_covers_every_string` fails when a string has no
translation here or an entry is no longer used.
