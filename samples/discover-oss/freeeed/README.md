# FreeEed processing investigation

This is local investigation toward running [FreeEed](https://github.com/shmsoft/FreeEed)
on ato.run. It is not a staging deployment or a complete eDiscovery application.
The inspected upstream commit is `d268fac3c59beb91b3d9d77d7c386657d149ac4d`.
The processing code carries Apache-2.0 notices, while the repository also
contains an EULA and separately licensed components. The complete distribution,
its activation flow, and each bundled dependency need separate review before
redistribution. Do not copy an upstream user's license file into an artifact.

On 2026-11-05 the parent POM and `freeeed-processing/src/main` at that commit
were built with Maven 3.9.11 and Java 17. The resulting 176 MiB JAR had SHA-256
`a02e7af58bdf0164bbc2b5bfe4ddb8eddbd407bc1ade6b32180f925d5fd68b3d`.
This is evidence for that local build, not a reproducibility guarantee: Maven
dependencies and build plugins were resolved from a repository, and no complete
dependency lock or license inventory was produced.

The upstream engine's text extraction calls a Tika REST server at
`localhost:9998`. With no Tika server, the engine logged a metadata extraction
failure but exited successfully and produced a ZIP containing `No text extracted`.
With an isolated Apache Tika 3.2.2 server on loopback, one synthetic plain-text
file was staged and processed; the resulting ZIP contained the original bytes
and the expected extracted text, and the single metadata row contained the
original file's MD5 and no processing exception. The tested Tika JAR had
SHA-256 `04bcb61826403957cfab93cb1e14d4c305fbeb949475f031b0b73dab7e976995`;
its SHA-512 matched the Apache release archive. These results do not establish
other file formats, OCR, email, indexing, review, isolation between users, or
production suitability.

`synthetic_check.py` repeats that narrow check on macOS using an existing
processing JAR and a caller-owned Tika server listening on port 9998:

```sh
python3 samples/discover-oss/freeeed/synthetic_check.py \
  .tmp/freeeed-build/freeeed-processing/target/freeeed-processing-1.0-SNAPSHOT-jar-with-dependencies.jar \
  .tmp/freeeed-run
```

The script creates a new test directory under the given root, confines the
engine's writes to that directory using `sandbox-exec`, and denies outgoing
connections other than loopback port 9998. It checks the output bytes and fails
if text extraction fails; it does not start or authenticate the Tika server.
The negative case without a Tika server and the successful case above both
passed on 2026-11-05. Only synthetic data was used.

`patch_upstream.py` checks the hashes of the two exact upstream source files
before adapting them: the Tika base URL becomes the JVM property
`freeeed.tika.base`, and the engine's hard-coded temporary directory uses
`java.io.tmpdir`. This is preparation for separately isolated services, not
proof of a hosted service group. The patched engine rebuilt offline using the
previously downloaded Maven dependencies and passed the same synthetic test;
the configured Tika URL was also exercised against the local test server.

`ProcessingPreview.java` is an experimental, synthetic-data-only upload and
download surface around that engine. It accepts one bounded file, uses a fresh
private job directory, and returns the metadata and native/text ZIP; it checks
the engine output instead of accepting its exit code alone. It is not the
upstream Review UI. `test_preview.py` starts isolated local engine and Tika
processes, verifies a synthetic text-file round trip and job cleanup, rejects
invalid requests, and verifies that an unavailable Tika produces an error:

```sh
python3 samples/discover-oss/freeeed/test_preview.py \
  .tmp/freeeed-build/freeeed-processing/target/freeeed-processing-1.0-SNAPSHOT-jar-with-dependencies.jar \
  .tmp/freeeed-tika/tika.jar .tmp/freeeed-run
```

That test passed on macOS on 2026-11-05. A local Chrome test also selected the
synthetic file and downloaded a result. The preview has not been tested under
the hosted Runner, reviewed for hostile files or concurrency, or published.
The complete source and dependency notices, all supported formats, stronger
failure reporting, and the upstream review/search workflow remain outstanding.

The upstream FastAPI example is not ready to expose directly. It uses an
uploaded filename and project name in server paths, accepts a custom project
file path, and queues work without a durable completion result. Its JSON
configuration writes `input-dir`, whereas the processing engine reads `input`.
An adapter must bound and validate paths, isolate each job and user, report
actual processing failures, and provide a complete user workflow. Review also
requires additional components beyond the processing engine and Tika. The
upstream project deletes its selected staging directory before processing;
any hosted adapter must control that directory and never accept arbitrary
server paths from a user.
