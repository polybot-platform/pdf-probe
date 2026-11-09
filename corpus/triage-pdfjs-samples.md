# Triage: pdf.js and poppler disagree on 25 of pdf.js's own test PDFs

Run `bun run samples` and its follow-ups first (see [README.md](README.md)). These are the 25 of 987 files where pdf.js 6.4.299 and poppler 24.02.0 `pdfinfo` gave different answers. For each one: what happened, what the library should do, and why.

The rule used: **fail closed, and match pdf.js unless pdf.js is wrong.** A library that is stricter than pdf.js costs a false rejection. One that is looser can report fewer pages or a smaller page than the reader that bills for them. The pdf.js test manifest (`test/test_manifest.json`) helped: a file registered as `eq` or `load` is one the pdf.js maintainers want read, and `password` entries name files that need one.

**Result: 23 files stay "match pdf.js"; 2 become intended differences** (listed in [`intended-differences.json`](intended-differences.json)). None of the seven files that pdf.js rejects and poppler reads is loosened.

## Intended differences (2)

| File | pdf.js | Library | Why |
|---|---|---|---|
| `Brotli-Prototype-FileA.pdf` | reads 25 pages | `unparseable` | The page tree sits in a Brotli-compressed object stream. `BrotliDecode` is a prototype filter, not in ISO 32000-2, and poppler fails on it too. The library supports Flate only, so an unknown filter on structure it needs is a failure. Revisit if Brotli is standardised |
| `poppler-85140-0.pdf` | 1 page, 612 x 792 | `unparseable` | The page object has an invalid header (generation number 18446744073709551616) and declares a MediaBox of `595 x 2147483647`. pdf.js reports the Letter default instead, so the size it returns is invented and hides a huge declared box. The rule: when a page object cannot be parsed, fail; never substitute a size. A page that parses but has no MediaBox still gets Letter, as in pdf.js |

## Match pdf.js (23)

**Password set: the library fails (4).** `bug1782186` (password "Hello" in the manifest), `issue15893_reduced` ("test"), `pr6531_2` and `encrypted-attachment` (AES-256 R6, `Identity` stream and string filters, only the embedded file is encrypted). pdf.js asks for a password. `pdfinfo` lists the page tree anyway; the likely reason is that the tree is not encrypted and `pdfinfo` does not need to authenticate to read it (not checked for each file). A file that needs a password is a failure, even when the page structure is readable.

**Fail closed where poppler is lenient (3).** `Pages-tree-refs` is a page-tree loop: poppler logs `Loop in Pages tree` but exits 0 with a `0 x 0` second page. `poppler-742-0-fuzzed` and `poppler-937-0-fuzzed` are fuzzed files: pdf.js fails, poppler returns one page of `0 x 0` after dictionary errors. A `0 x 0` page is a recovery artifact, not a size.

**Invalid CropBox falls back to MediaBox (1).** `boundingBox_invalid`: page 3 has a CropBox that does not overlap its MediaBox. pdf.js uses the MediaBox (600 x 800); poppler returns `0 x 0`, which would slip under any size limit. Falling back to the larger box is the safe direction.

**RC4 with a 40 or 48-bit key under V4 (2).** `issue19484_1` and `issue19484_2`: `/V 4 /R 4`, crypt filter `CFM /V2` with `/Length 5` and `/Length 6`, `/EncryptMetadata false`. pdf.js decrypts them with an empty password. poppler fails with `Unknown compression method in flate stream`. Confirmed by building the same kind of file: under V4, a key shorter than 16 bytes is zero-extended to 16 before the per-object keys are derived (not in the spec; pdf.js does it for exactly these two files). poppler skips that step. The library must take the key length from the crypt filter. **Fixtures written:** `enc-rc4-v4-40-*` and `enc-rc4-v4-48-*` (see `generate-modern.ts`), including the zero-extension.

**Counts that poppler rejects (3).** `poppler-67295-0` (`/Count 9999999999`, one page) and `poppler-91414-0-53` / `-54` (`/Count` is a reference to a stream whose `/Length` refers to itself). pdf.js falls back to walking the tree and reports 1 page. Reporting the real count is safe, because it can only be lower than a lying `/Count`. This is the over-count rule already in [README.md](README.md); Gemini was not tested on an over-count.

**Real-world structure that poppler cannot read (6).** pdf.js maintainers added all of these as regression tests, so reading them is the intended outcome.
- `issue9418`: a hybrid file (`/XRefStm` plus `/Prev`), 1 page of 3023.758 x 2303.816, so `pageSize`. **Fixtures written:** `hybrid-prev-xrefstm-*` reproduce its structure (empty `0 0` subsection, trailer with `/Prev` and `/XRefStm`, a stale `/Prev` table that the stream shadows, a dense `/Index`-less stream). Poppler reads those, so the cause of its failure on the real file is still unknown.
- `bug1978317` (xref stream, 1.5 MB) and `issue18986` (xref stream, `type: load`): poppler reports a missing trailer or stream length.
- `issue9105_other` and `bug1980958`: no usable xref, recovered by scanning. `issue15590`: the trailer comes before the objects and the "Pages" node is an action dictionary with no `/Kids`, read as one page. These follow the rule that a node without `/Kids` is a page, and a page with no MediaBox gets Letter.

**Limits of `pdfinfo`, not of the files (4).**
- `/UserUnit` is ignored by `pdfinfo`: `issue19176` (`/UserUnit 72`) and `bug1947248_forms` / `bug1947248_text` (`/UserUnit 3`, 612 x 792 becomes 1836 x 2376 in pdf.js).
- Rounding: `issue9949` is 1417.323 in pdf.js and 1417.32 in `pdfinfo`, which prints two decimals.

## What this adds to the plan

- Two intended differences, already in `intended-differences.json`.
- Both fixtures are written (RC4 short-key and the `issue9418`-shaped hybrid files). The hybrid ones do not reproduce poppler's failure; that cause is still unknown.
- The "never invent a size" rule for unparseable page objects (above).
- Gemini's behaviour was not tested on any of these. The recovery cases in particular rest on "pdf.js can read it", not on what the billing reader does.

## Added with the recovery scan

Two more files that pdf.js reads and the library refuses, found when the scan made the last refusals visible. Both are stricter, never looser, and are in [`intended-differences.json`](intended-differences.json).

| File | pdf.js | Library | Why |
|---|---|---|---|
| `bug1250079.pdf` | reads 1 page, 200 x 50 | `unparseable` | Every dictionary, the trailer included, has lost its `<<` and `>>`. No usable trailer is found |
| `PDFBOX-4352-0.pdf` | reads 1 page, 200 x 50 | `unparseable` | An AES-256 file whose encryption dictionary is corrupted. pdf.js ignores the unreadable dictionary and reads the file as unencrypted. A file that says it is encrypted, and whose encryption cannot be read, has not been shown to be readable |

