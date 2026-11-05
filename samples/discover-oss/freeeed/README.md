# FreeEed processing investigation

This investigates running [FreeEed](https://github.com/shmsoft/FreeEed) on
ato.run. A narrowly scoped text-only preview has been tested in a private
staging Instance; it is not a complete eDiscovery application.
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
dependency lock or license inventory was produced at that point.

The later `dependency-inventory.json` records 217 resolved compile/runtime
artifacts, including six classifier-specific artifacts, with their local JAR
SHA-256 values and available POM license fields. Seven have no license field
in the inspected POM/parent chain. For example, the Stanford CoreNLP 4.4.0 POM
names GPL-3.0, and the AppleJavaExtensions 1.4 POM names an Apple license. This
is a dependency-resolution inventory, not proof of which bytes are present in
the assembly or of redistribution compliance; it does not supply the required
license texts or corresponding source. `make_dependency_inventory.py` creates
it from a Maven dependency tree and the exact local repository.

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
download surface around that engine. It accepts one bounded UTF-8 text file, uses a fresh
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

## Local OCI service-group candidate

`assemble_candidate.py` assembles an **unpublished** candidate under `.tmp`
from the pinned upstream Git checkout and a previously patched Maven build. It
splits the processing JAR into files below the portable-workspace per-file
limit and includes the patched engine source JAR, POMs, Apache license, NOTICE
and adapter sources. This is not yet a complete dependency-license inventory.
`start.sh` verifies the reassembled engine digest before using it; the engine
cache and per-job files are inside the declared `/data` slot.

On 2026-11-05 the candidate packed successfully as a 248,111,386-byte Capsule
with bundle SHA-256 `c6a0e0b0fbb008673b269701522825d64d312c23113b246082f712591faa7ba0`,
ContractRef `7ea9030861cc59692a70ac22d59cfea1cecc93991220aaf807a2ae1e402f157a`
and DerivationRef `ab7001c69e603f8b413559e8fad27e1e9986568ccf71c80a4e90998dd423ad0b`.
It requires the direct-upload path because it exceeds the 32 MiB Worker
upload limit. A local Docker Desktop test used the two digest-pinned linux/amd64
images, a private internal network, non-root users, read-only container roots,
resource limits and a writable data directory. Through a loopback-only test
forwarder, health passed and the synthetic text document produced the expected
original and extracted text; the job directory was removed. This is Docker
evidence, not an ato Runner receipt. On the macOS test host, `ato run` refused
the route because OCI service groups require a native Linux Docker host.

On 2026-11-05 the candidate was submitted in the staging import UI with the
owner's Connected Runner selected. The UI returned `validator_failed`; it did
not expose the validator's detailed error or create an app. The current API
source limits the validator's uploaded workspace artifact to 64 MiB, while the
three engine chunks alone total more than 175 MiB. The observed error is
consistent with that limit, but the deployed code and exact rejection cause
were not independently observed. Uploading the same candidate again without a
change would not establish the cause.

This candidate has not been imported or published on staging. Its preview
does not provide FreeEed Review, search, OCR, or a complete eDiscovery
workflow. Do not treat this experiment as a safe place for real evidence or
confidential documents. The large cached engine also makes saved-data and
checkpoint behavior an explicit outstanding test.

## Smaller text-only experiment

`trim_text_preview.py` can produce a smaller engine for this preview. It checks
the input JAR digests and deliberately removes libraries used by features the
preview does not offer, including Hadoop, Solr, Stanford CoreNLP, AWS, PDFBox,
Apache POI and non-target SQLite native binaries. The resulting JAR is not a
full FreeEed distribution. The web surface accepts only valid UTF-8 `.txt`
files and explicitly rejects other extensions and binary control bytes. The
source ZIP includes the upstream processing sources, adapter, patch and
trimming scripts, POMs, and the preliminary dependency inventory; dependency
license texts and corresponding third-party sources are still incomplete.
Do not publish it as fully compliant or as a replacement for FreeEed.

The 2026-11-05 text-only candidate contains 50,949,340 bytes of workspace
files and a 67,942,062-byte Capsule. Its bundle SHA-256 is
`ce1fd2b9282970ddcea70cae922fea5b39d0c34cfe535d34d98516d4de8baac4`,
ContractRef is `7ea9030861cc59692a70ac22d59cfea1cecc93991220aaf807a2ae1e402f157a`,
and DerivationRef is `f6a4b87a075c2664c5b691943d34bc822d7183e83c3ce647c35dab2c5f7112ff`.
The exact assembled workspace passed a local Docker test with the same
non-root, read-only, isolated two-service setup described above. The test
checked health, source download, synthetic text processing and job cleanup.
The local native test additionally rejected binary and non-TXT inputs and
verified failure when Tika was stopped. This remains an experiment, not a
hosted verification or a complete license review.

On 2026-11-05 the text-only Capsule passed the staging format validation but
the owner-selected Connected Runner import ended in `hosted_contract_failed`.
The UI did not show a failed observation or runtime log and no app was created.
This result does not establish whether either container started or what caused
the failure. The local Docker success above must not be presented as a hosted
success.

`retained-inventory.json` compares the trimmed JAR against the 217 resolved
dependency JARs by matching each entry's bytes. It found matching entries in
134 artifacts; seven still lack POM license metadata. Byte matching is useful
evidence but is not a complete binary/source provenance or license review.

## Single-container text experiment

`single-text.capsule.toml` and `start-single-text.sh` use the same trimmed
FreeEed processing engine with `TextOnlyExtractor.java` in one container. The
extractor listens only on loopback and implements just the two responses the
engine needs for validated UTF-8 text; it is **not** Apache Tika and cannot
process other document formats. The app says so, accepts only sample data,
and remains incomplete as an eDiscovery tool. It avoids a second service to
test a different hosted execution route; it does not establish the cause of
the preceding service-group failure.

On 2026-11-05, both the original Tika-backed preview and the text-only adapter
passed the synthetic input, output, extractor failure, and cleanup test using
`test_preview.py` (add `--text-only` for the adapter). The exact single-service
workspace also passed a local isolated Docker test with a read-only root,
non-root user, bounded resources, and no external network. It returned the
original synthetic document and extracted text and removed the temporary job.
The portable bundle SHA-256 is
`71c80709d277646af8ffa0b3888921a153cecb88bdf86ccec634d771181aaf59`, its
ContractRef is `e8614c1f6588857a76305ceb920703687e9ed52fef0635afeb72a414885fc819`,
and its DerivationRef is `a3c4acc111c25526990233f87308e83800520f6a6df4c2a4a1530dd8cfd2bda7`.
The third-party license/source review remains incomplete; do not publish this
candidate as a finished FreeEed service.

The same single-container bundle reached staging Instance
`cinst_01M6NNC72FKGJM4A76FGDSD1BT` on the owner's connected Runner on
2026-11-05. Its ready page showed the OCI Contract verified. In the owner
browser, the notice identified the direct text adapter and a synthetic TXT
file reached the "Results downloaded" state. The returned file's contents
were verified in the local Docker test, not independently in staging. The
Instance remains unpublished while dependency notices and source provenance
are reviewed; this is not FreeEed Review or a full-format eDiscovery service.

For a smaller text-only candidate, `trim_text_preview.py` accepts
`--single-text-extractor` and removes additional mail, PDF, XML and other
unused classes. This intentionally further narrows the application. The
resulting pinned engine SHA-256 is
`57f3e39f0cfe4aa9b85bcec4411b91db69706386f27fe2b280c0f82b48327d1a`.
The synthetic success, malformed-input and extractor-failure tests passed.
The preview replaces SQLite JDBC with 3.41.2.2, Guava with 33.8.0-jre and
failureaccess 1.0.3, and Commons IO with 2.14.0. Their downloads are pinned
by `fetch_replacement_jars.py`. Unused classes are removed and merged service
registrations are rewritten to retain only providers present in the preview.
`single-text-inventory.json` records 91 candidate dependencies. The separate
`audit_trimmed_provenance.py` matched all 17,556 retained non-metadata files
and metadata class files to the pinned application or Maven JARs; the report
does not claim that merged non-class `META-INF` files were independently
attributed. All 91 candidate POMs contain license metadata. This is not a
complete security audit; the preview remains restricted to synthetic text.

`collect_dependency_sources.py` downloads pinned-version Maven source
artifacts, and `fetch_extra_licenses.py` retrieves hash-checked upstream
license texts. `package_dependency_notices.py` checks the binary and source
digests and packages the notices, supplemental license texts, source URLs,
source hashes and provenance report. `assemble_candidate.py --single-text
--engine <engine> --notices <notices.zip>` checks that the notice bundle names
the exact engine and includes it, the adapter's MPL-2.0 license, the upstream
Apache license and adapter sources in the app's source download. This evidence
does not establish correctness for arbitrary documents or a full legal audit.

The updated candidate bundle SHA-256 is
`ce8adfd5a7ab074c323477b48542af97f67dfe8027289da73bf1c7455a214677`.
On 2026-11-05, staging Instance `cinst_01M6NR95TNK438HD86NE543XYJ`
on the owner's connected Runner reported the OCI Contract verified. The
owner browser processed a synthetic TXT and displayed "Results downloaded";
the downloaded bytes were checked in local Docker, not independently on
staging. An attempt to publish this instance to Discover was refused with a
message that its build was not ready, so this instance is private and no
public availability is claimed. The response does not establish the cause.

On 2026-11-06, the hosted validator from
[ato #1696](https://github.com/ato-run/ato/pull/1696) was deployed to staging.
Using the same `freeeed-safe-text.capsule` and the owner's connected personal
Runner, a new staging Instance `cinst_01M6P4CVS2MYZAPQHK5J3HZS48` reported
the OCI Contract verified and its surface ready. The owner browser opened
`https://cinst-o2g4qlqbxmigpigg.stg-app.ato.run/`, processed a synthetic TXT,
and displayed "Results downloaded". The downloaded ZIP and its nested native
ZIP passed integrity checks. Both the extracted text and original native file
matched the 54-byte synthetic input, and the metadata row recorded their MD5
`8afd6b0f1099b49266db9448115296f7` without a processing exception. This is one
synthetic text case, not evidence of other formats or a complete review flow.
Discover publication was refused with the build-not-ready
message even after this validator deployment, so the Instance remains private.
The UI message alone does not identify the exact server-side refusal or prove
whether ingress evidence was recorded. An earlier managed-Runner import was
refused as `portable_runner_unavailable`; selecting the connected personal
Runner allowed this separate attempt to start.

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

## Review and indexing local integration

On 2026-11-05 the separately maintained upstream
[FreeEedUI](https://github.com/shmsoft/FreeEedUI) Apache-2.0 repository was
inspected at commit `dbe04fcc8cecc4634894d81eeb684d558031912a`. Its original
WAR built with Maven 3.9.11 and Java 17; the resulting local WAR SHA-256 was
`7745058a31069a8d82e7f61f08b2be8117ff245b6e9ed92b7b3995932de63a36`.
The build resolved dependencies from Maven Central without a complete lock, so
this hash is evidence for that build only, not a reproducibility claim.
The upstream release script downloads a customized Solr 4.0.0 archive; the
inspected archive SHA-256 was
`f46814b984a01aeb0548205e7d1cd5cd4f5477d95d34d11fbfdc934f9f0354de`.
No upstream checksum was available from that release script.

With all services sharing a Docker network namespace with **no external
network**, the patched full processing engine, Tika 3.2.2, the upstream Solr,
and the original review WAR ran as non-root with read-only container roots.
The engine processed one synthetic UTF-8 file, created its Solr core and review
case, and the review search returned the corresponding indexed document. The
SQLite native library was extracted to a writable executable test directory;
this local arrangement is not suitable for an unreviewed hosted release.

The original review app initializes a known administrator password and stores
user passwords without hashing. Its internal case-creation endpoint was also
observed to create a synthetic case without a session. Controllers store
per-request fields on singleton Spring beans. These concrete issues, legacy
Solr and dependencies, direct file-path handling, source/notices, state backup,
resource bounds, and user authentication require review before deployment.
The test did not use real customer data and did not publish this full workflow.

`review/prepare_candidate.py` verifies the four affected source file digests,
then prepares a **local-only** candidate that creates per-request controller
instances and permits automatic case creation only for a loopback POST carrying
a per-instance internal token. Ordinary case access still requires the
application's case permission. `review/prepare_engine_case_creator.py` verifies
the original engine source digest, then prepares its matching client: it sends
the token only to the fixed local review endpoint, rejects redirects, and
requires a bounded success response naming a case ID. `CaseCreatorProbe.java`
exercises response and destination restrictions against a synthetic loopback
server. The modified engine and review app locally rejected missing/wrong tokens
and unauthorized case operations, and processed, indexed, registered and
searched a new synthetic document with the correct token. A token-absent engine
run did not create a review case. The local-only candidate additionally requires an initial administrator password
from `ATO_BINDING_INITIAL_ADMIN_PASSWORD`, removes the prefilled password, and
stores salted PBKDF2-HMAC-SHA256 verifiers with 600,000 iterations rather than
plaintext. This iteration count follows the [OWASP password storage guidance](https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html).
The user store is written with owner-only permissions by atomic replacement;
existing plaintext credentials are rejected without rewriting their store.
Local tests checked default and incorrect credentials, successful login, a
restart without the initial password, and a legacy store that remained unchanged
after startup failed. A fresh synthetic document was processed and indexed,
and authenticated review search returned it. These observations are not a
complete security review. No token or user password is included in the source
or build inputs.

The two preparation commands accept an extracted pinned source tree and write
a new destination; they refuse unexpected input hashes or an existing output:

```sh
python3 samples/discover-oss/freeeed/review/prepare_candidate.py <review-source> <new-review-directory>
python3 samples/discover-oss/freeeed/review/prepare_engine_case_creator.py <upstream-AutomaticUICaseCreator.java> <new-java-file>
```

For the local experiment the two processes received the same freshly generated
`FREEEED_INTERNAL_CASE_TOKEN`, and the review endpoint was restricted to
loopback port 8090. A production or stg implementation would need a protected,
per-Instance binding and lifecycle for that token, rather than putting it in a
Capsule or logs. Test-only instrumentation confirmed that repeated requests
were dispatched to different controller objects. Other shared objects and the
full authentication and file-upload paths have not been audited. In particular,
rate limiting, CSRF, dependency vulnerabilities, legacy credential migration,
and backup and recovery still require work. A prototype Java serialization
filter limits the accepted user-store classes, graph and file size; a synthetic
probe rejected an unexpected class before its deserializer executed and rejected
an oversized array. Reloading the previously saved synthetic store and reviewing
its indexed document succeeded. Deleting the initial administrator is rejected
(currently as HTTP 500); an existing store without that administrator fails
closed. Recovery and a usable deletion error remain to be designed.
