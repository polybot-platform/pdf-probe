# How pdf-probe gets the page count and page sizes

> **This is the full design, and the code implements all of it.** Hardening is partly done: a mutation fuzzer has run about 28 million inputs without a panic, a hang or a memory spike, and a coverage-guided libFuzzer target exists but has not been run. Packaging and a measurement in a container are not done. Each step below is what the library must do to match pdf.js on the [corpus](../corpus/README.md); the rules come from pdf.js's behaviour, the PDF specification, and the findings in [`corpus/triage-pdfjs-samples.md`](../corpus/triage-pdfjs-samples.md). The recovery scan runs when no cross-reference section can be read, when the catalog or page tree root is not a dictionary, or when fetching the first or last page hits a bad entry, as in pdf.js.

The library reads only what it needs: the end of the file, the cross-reference data, the catalog, the page tree and the page dictionaries. It does not touch content streams, fonts or images, and it does not read every object in the file.

```mermaid
flowchart TD
    IN["PDF bytes + page cap"] --> S1["1. Find the newest cross-reference section<br/>read startxref from the last 1 KB"]
    S1 --> S2["2. Read the xref chain<br/>classic table or xref stream (Flate + PNG predictor)<br/>follow /XRefStm, then /Prev<br/>newest entry wins; cycle and length limits"]
    S2 -->|"missing, damaged, or inconsistent"| REC["Recovery scan<br/>find 'N G obj' and trailers in the bytes<br/>bounded; same outcomes as pdf.js"]
    S2 --> S3["3. Trailer: /Root and /Encrypt"]
    REC --> S3
    S3 -->|"/Encrypt present"| ENC["Derive the key with an EMPTY password<br/>RC4, AES-128, AES-256 (R6)<br/>V4 key under 16 bytes: zero-extend to 16<br/>a password is required: failure"]
    S3 -->|"not encrypted"| S4
    ENC --> S4["4. Catalog, then the root /Pages node<br/>objects may sit in object streams:<br/>inflate within the size limit, then parse"]
    S4 --> S5["5. Page count<br/>root /Count usable: trust it<br/>/Count invalid or over-counts: walk the tree and count pages<br/>cycle detection and depth limit"]
    S5 -->|"0 pages"| EMPTY(["failure: empty"])
    S5 --> S6["6. Page sizes: visit each page<br/>inherit MediaBox, CropBox, Rotate down the tree<br/>MediaBox ∩ CropBox (no overlap: use MediaBox)<br/>swap width and height for Rotate 90 / 270<br/>multiply by UserUnit"]
    S6 --> OUT(["result: page count + each page size in points"])
    S1 -.->|"any step fails or hits a limit"| FAIL(["failure: unparseable"])
    S2 -.-> FAIL
    S4 -.-> FAIL
    S5 -.-> FAIL
    S6 -.-> FAIL
```

## Notes on the rules

- **Fail closed.** If a step cannot be completed with confidence, or a limit is reached (object count, recursion depth, cross-reference chain length, decompressed size), the result is a failure. The library never guesses a page count and never invents a page size for a page object it cannot parse.
- **`/Count`.** pdf.js trusts a valid `/Count` and counts the real pages only when `/Count` is invalid or larger than the tree. Gemini reads and bills exactly `/Count` pages, so an under-count is not a way past a page cap. See [`corpus/README.md`](../corpus/README.md).
- **Failure kinds.** `unparseable` (damaged, unsupported, password required, limit hit) and `empty` (well-formed, no pages). A wrapper with a fixed set of kinds maps `empty` to `unparseable`.
- **Where it differs from pdf.js.** Listed, with reasons, in [`corpus/intended-differences.json`](../corpus/intended-differences.json): an empty page tree is a failure, and a Brotli-compressed structure is a failure.
- **Not yet measured.** Memory and speed of the real library: nothing to measure until it exists.
