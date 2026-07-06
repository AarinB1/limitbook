# ITCH 5.0 specification

`itch50_spec.txt` is the committed, text-extracted copy of the official
**Nasdaq TotalView-ITCH 5.0** specification. It is the source of truth for
every message layout and field offset in this project — code and tests must
cite it, never memory.

## Provenance

- Source: <https://www.nasdaqtrader.com/content/technicalsupport/specifications/dataproducts/NQTVITCHspecification.pdf>
- Retrieved: 2026-07-06 (PDF last-modified 2025-09-23, 36 pages)
- PDF sha256: `45e0531d1b4b3beb886e9618b2ab824a5aa9bda3a99c0dff03509306e68aacc3`
- Extraction: `pdftotext -layout NQTVITCHspecification.pdf itch50_spec.txt` (poppler 24.02.0)

The layout-preserving extraction keeps the message tables' Name / Offset /
Length / Value columns intact. If a table ever reads ambiguously, re-check
against the PDF at the URL above before relying on it.

## Framing note

The message layouts in the spec describe payloads only. Nasdaq's
downloadable sample-day files (BinaryFILE convention) prefix each payload
with a 2-byte big-endian length; that framing is handled by
`limitbook-core::frame` and by the fixture tooling.
