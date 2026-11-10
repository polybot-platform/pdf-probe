# How pdf-probe works

The library reads only what it needs: the header and tail of the file, the cross-reference data, the trailer, the encryption dictionary (if any), the catalog, the page tree, and the page dictionaries. It never touches content streams, fonts, or images, and never parses unreferenced objects unless the cross-reference is damaged and must be reconstructed.

```mermaid
flowchart TD
    IN["PDF bytes + Limits"] --> S1["1. Check max_input_bytes & find %PDF- header<br/>Read startxref from the last 1 KB (or scan backwards)"]
    S1 --> S2["2. Read the xref chain<br/>Classic table or xref stream (Flate + PNG predictor)<br/>Follow /XRefStm, then /Prev<br/>Newest entry wins; cycle and length limits"]
    S2 -->|"missing, damaged, or inconsistent"| REC["Recovery scan (repaired = true)<br/>Find 'N G obj', trailers, and /XRef streams<br/>Bounded by work meter; same rules as pdf.js"]
    S2 --> S3["3. Trailer: /Root and /Encrypt"]
    REC --> S3
    S3 -->|"/Encrypt present"| ENC["Derive key with EMPTY user password (encrypted = true)<br/>RC4, AES-128, AES-256 (R5 & R6)<br/>V4 key under 16 bytes: zero-extend to 16<br/>Non-empty user password required -> Failure::Unparseable"]
    S3 -->|"not encrypted"| S4
    ENC --> S4["4. Catalog (/Version, /Pages) -> root /Pages node<br/>Objects in /ObjStm inflated within max_decompressed"]
    S4 --> S5["5. Page count<br/>Valid root /Count: trust it (unless larger than actual tree)<br/>Walk page tree with cycle detection and depth limit (64)"]
    S5 -->|"0 pages"| EMPTY(["Failure::Empty"])
    S5 -->|"> max_pages"| MANY(["Failure::TooManyPages"])
    S5 --> S6["6. Page sizes & flags<br/>Inherit MediaBox, CropBox, Rotate down the tree<br/>view = MediaBox ∩ CropBox (cropped = true if view != MediaBox)<br/>Swap width/height for Rotate 90 / 270; multiply by UserUnit"]
    S6 --> OUT(["Probe: pages, sizes, work, version, encrypted, repaired, cropped"])
    S1 -.->|"limit reached"| LIM(["Failure::LimitExceeded"])
    S2 -.->|"limit reached"| LIM
    S4 -.->|"limit reached"| LIM
    S6 -.->|"limit reached"| LIM
```

## Core rules

- **Fail closed.** If a step cannot be completed with confidence, the result is `Failure::Unparseable`. If any resource limit (`max_input_bytes`, `max_work`, `max_objects`, `max_xref_sections`, `max_decompressed`) is reached, the result is `Failure::LimitExceeded` and no recovery rescan is attempted.
- **`/Count` handling.** Like `pdf.js` and poppler, `pdf-probe` trusts a valid non-negative integer `/Count` on the root `/Pages` dictionary and only walks the full tree beyond `/Count` when `/Count` is missing or invalid. If the tree has fewer pages than `/Count` claims, the actual lower count is returned.
- **Intended differences from `pdf.js`.** Listed in [`corpus/intended-differences.json`](../corpus/intended-differences.json): an empty page tree returns `Empty`, and Brotli-compressed streams or unparseable page objects fail closed rather than substituting a default page size.
