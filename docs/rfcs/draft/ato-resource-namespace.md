---
title: "RFC: Ato Resource Namespace"
status: draft
date: "2026-06-07"
author: "@egamikohsuke"
ssot: []
related:
  - "../../research/ato-resource-namespace-asset-inventory.md"
---

# RFC: Ato Resource Namespace

## Status

Draft.

## Summary

Ato Resource Namespace は、Ato ManagedCloud / Desktop Runner / External Runner / Browser Runner / Store / Runtime Network を横断して、Runner / Capsule / Instance / Session / Artifact / Receipt / Secret / State を control plane 内部の typed resource tree として扱うための論理名前空間である。

## Motivation

Ato は capsule を同じハンドルで宣言・展開・実行・修復する基盤である。Desktop だけで完結していた段階では、実行先、状態、ログ、secret、artifact を個別の概念として扱っても成立した。しかし ManagedCloud、Desktop Runner、External Runner、Browser Runner、BYOC が増えると、Runner と Session の解決が各実装に分散しやすくなる。

Store install、Launch Profile、State、Secrets、Receipts、Logs、Artifacts はそれぞれ必要な概念だが、別々のキー体系で扱うと「どの capsule の、どの install の、どの runner 上の、どの session に紐づくものか」が不安定になる。Ato Resource Namespace はこれらを公開 URL scheme ではなく control plane 内部の stable path で接続する。

Execution Identity は launch envelope の identity であり、resource tree そのものではない。`execution_id` は「ある session がどの launch envelope で起動されたか」を表すが、namespace は「どこに何があるか」を表す。両者を混ぜると、状態観測や診断ログの変化まで execution drift と誤認する危険がある。

PlacementGraph は配置判断をするが、その入力である runner candidates と、出力である chosen runner/session を安定して指す namespace が必要である。`/runners/<runner_id>` を placement input、`/sessions/.../runner` を placement output として扱うことで、配置判断、receipt、audit を同じ model に接続できる。

Mobile / Web Console / Desktop Runner / ManagedCloud では、ユーザーから見て「どのデバイス・どのクラウド・どのセッションで動いているか」を統一的に扱う必要がある。Resource Namespace は UI のための表示階層ではなく、その UI が参照する control plane の正規 model になる。

## Non-goals

- 公開 URL scheme ではない。
- `capsule://` の代替ではない。
- `ato://` の代替ではない。
- FUSE / 9P / WebDAV の実装を v0 で要求しない。
- Kubernetes / Nomad の再実装ではない。
- Runtime Network 全体の仕様ではない。
- Execution Identity の再定義ではない。
- Secret の実値を namespace node に保存しない。
- v0 では arbitrary TCP tunnel を要求しない。
- Runner の OS sandbox policy をこの RFC だけで定義しない。
- Store の公開 catalog URL や publisher-facing routing を定義しない。

## Target Model

Ato Resource Namespace の v0 論理 tree は次を初期形とする。

```text
/runners
  /<runner_id>
    /metadata
    /capabilities
      /latest
      /<snapshot_id>
    /health
    /commands
    /sessions
      /<session_id>

/devices
  /<device_id>
    /metadata
    /runners
      /<runner_id>

/runtime_networks
  /<runtime_network_id>
    /metadata
    /dns
      /<record_id>

/storage
  /<storage_id>
    /metadata
    /credentials
    /mounts

/auth
  /sessions
    /<auth_session_id>
  /grants
    /<grant_id>
  /credentials
    /<credential_id>

/consents
  /<consent_id>

/network_policies
  /<network_policy_id>

/capsules
  /github.com/<owner>/<repo>/<commit>
  /ato.run/<publisher>/<app>/<version>

/instances
  /<install_profile_key>
    /revisions/<install_revision_id>
    /profiles/<profile_id>
    /state
    /secrets
    /sessions

/sessions
  /<session_id>
    /runner
    /ports
    /logs
    /receipt
    /execution_identity

/artifacts
  /blake3/<hex>
/receipts
  /<receipt_id>
/logs
  /<log_id>
/secrets
  /<secret_ref_id>
```

各 node は typed metadata を持つ。path は identity の索引であり、node payload の全体を path segment に詰め込んではならない。

| Node | Kind | 役割 |
|---|---|---|
| `RunnerNode` | `runner` | ManagedCloud / Desktop Runner / External Runner / Browser Runner / BYOC の登録、能力、health を表す。 |
| `DeviceNode` | `device` | ユーザーから見える端末または control endpoint を表す。実行能力は Runner に分離する。 |
| `RuntimeNetworkNode` | `runtime_network` | owner subject / workspace / project ごとの private runtime network を表す。 |
| `RuntimeDnsRecordNode` | `runtime_dns_record` | runtime network 内の stable name から device / runner / session route への private DNS projection を表す。 |
| `StorageNode` | `storage` | object store、S3互換、volume、database、KV/cache などの外部 storage resource を表す。 |
| `AuthSessionNode` | `auth_session` | Web Console、mobile、runner join などの認証済み session を表す。 |
| `AccessGrantNode` | `access_grant` | subject/device/runner が namespace operation を行う権限を表す。 |
| `ConsentGrantNode` | `consent_grant` | app requirement が resource に binding されることへの user consent を表す。 |
| `NetworkPolicyNode` | `network_policy` | proxy-only、allowlist、runner policy など network requirement の materialized policy を表す。 |
| `CapsuleNode` | `capsule` | Store listing、GitHub source、versioned capsule の canonical 参照を表す。 |
| `InstanceNode` | `instance` | install profile key を root にした、ユーザーが扱う installed capsule 単位を表す。 |
| `RevisionNode` | `revision` | install revision と artifact build の対応を表す。 |
| `LaunchProfileNode` | `launch_profile` | launch envelope の入力になる profile を表す。 |
| `StateNode` | `state` | instance-scoped state contract と backing store binding を表す。 |
| `SecretRefNode` | `secret_ref` | secret value ではなく、secret manager 上の参照と権限境界を表す。 |
| `SessionNode` | `session` | 実行中または終了済みの execution session を表す。 |
| `PortNode` | `port` | session route に紐づく published/local/runner port binding を表す。 |
| `LogNode` | `log` | session log stream または log object ref を表す。 |
| `ReceiptNode` | `receipt` | execution receipt、placement decision、observed result への参照を表す。 |
| `ArtifactNode` | `artifact` | `.sync` archive、OCI image、Wasm bundle、source snapshot などの content-addressed artifact を表す。 |
| `LinkNode` | `link` | canonical node への alias / back reference / attached reference を表す。 |

`/runners/<runner_id>` は opaque global id を canonical path に使う。`desktop`、`managed`、`external`、`browser`、cloud provider、region、machine id は path segment に焼き込まず、`/runners/<runner_id>/metadata` の typed payload に置く。

`ato-vfs` の `VirtualFileSystem` / `MountTable` / `SecurityManager` / `ResourceMonitor` は、この tree を実装するときの直接材料である。ただし元実装は WASM runtime 内 VFS なので、control plane 内部では FUSE backend ではなく `ResourceBackend` trait と path validation の考え方を抽出する。

`sync-rs` の `VfsMount` / `VfsEntry` は `.sync` archive を path-based mount に写す設計として採用する。v0 では physical mount ではなく、`CapsuleArtifact` と `StateBinding` の shape に反映する。

## Subject Scope and Global Uniqueness

v0 は top-level tree を維持する。`/subjects/<subject_id>/...` を canonical path には入れない。代わりに、control plane が mutable resource id を global unique にし、`owner_subject` を必須 invariant として保持する。

この RFC の v0 invariant:

- `/instances/<install_profile_key>` の `install_profile_key` は control plane global unique である。
- `/sessions/<session_id>` の `session_id` は control plane global unique であり、runner-local process id ではない。
- `/secrets/<secret_ref_id>` の `secret_ref_id` は control plane global unique な opaque id であり、secret value や runner token を含まない。
- `/receipts/<receipt_id>` と `/logs/<log_id>` は control plane global unique である。
- すべての mutable node は `NamespaceAcl.owner_subject` を必須にする。
- ACL 評価は path だけでなく、`owner_subject`、caller subject、operation、runner trust、consent scope を同時に見る。
- 将来 `/subjects/<subject_id>/instances/...` のような subject view を追加する場合、それは canonical node の複製ではなく `ResourceLink` による view とする。

`/capsules/...` と `/artifacts/...` は immutable / content-addressed resource になりうるため、必ずしも単一 subject に閉じない。ただし install、session、secret、receipt、log に接続される時点で owner subject と consent scope を持つ。

## Link and Mount Semantics

Resource Namespace 内で同じ実体を複数 path から見せる場合は、複製ではなく `ResourceLink` を使う。canonical node、alias、back reference、attached reference を区別しないと、ACL、receipt、cleanup、audit の正本が曖昧になる。

v0 の canonical node:

- session の正本は `/sessions/<session_id>`。
- receipt の正本は `/receipts/<receipt_id>`。
- log object の正本は `/logs/<log_id>`。
- artifact の正本は `/artifacts/<algorithm>/<hex>`。
- secret ref の正本は `/secrets/<secret_ref_id>`。

v0 の link semantics:

- `/instances/<install_profile_key>/sessions/<session_id>` は `/sessions/<session_id>` への `BackReference` であり、session payload の複製ではない。
- `/sessions/<session_id>/receipt` は `/receipts/<receipt_id>` への `AttachedReference` であり、receipt payload の正本ではない。
- `/sessions/<session_id>/logs/<log_id>` は `/logs/<log_id>` への `AttachedReference` である。
- `/instances/<install_profile_key>/secrets/<secret_ref_id>` は `/secrets/<secret_ref_id>` への `BackReference` であり、secret value を持たない。
- shortcut、dashboard、mobile control surface の表示用 path は必要なら `Alias` として作る。alias は canonical path comparison の正本にならない。

Link ACL evaluation:

- `get(link_path)` は link node 自体の `Get` を確認してから link payload を返す。
- `resolve(link_path)` は link node の `Resolve` を確認し、さらに canonical target に対する実 operation の ACL を確認する。
- `list(parent)` で link を表示する場合は parent/list 権限と link node/list visibility を確認する。
- link 経由で target を操作する場合、link ACL と target ACL の両方を満たす必要がある。これにより alias 経由の confused deputy を避ける。

Deletion and tombstone:

- v0 では canonical node を原則 hard delete しない。削除は tombstone state として記録する。
- tombstoned canonical node への link は dangling link として残ってよいが、`resolve` は `Tombstoned` を返す。
- GC は tombstone、link、audit、receipt retention policy を確認してから行う。
- `AttachedReference` は canonical node への参照であり、`mount(path, backend)` operation とは別概念である。backend を結びつける operation は将来 `bind_backend(path, backend)` へ rename してもよい。

## Core Types

以下は実装コードではなく RFC 用 pseudo-code である。Rust 実装、TypeScript API、DB schema のいずれに置く場合も、同じ domain boundary を維持する。

```rust
struct ResourcePath {
    raw: String,
    canonical: String,
    segments: Vec<ResourceSegment>,
}

struct ResourceSegment(String);

struct ResourceRef {
    path: ResourcePath,
    expected_kind: Option<ResourceNodeKind>,
    version: Option<String>,
}

struct ResourceNode {
    path: ResourcePath,
    kind: ResourceNodeKind,
    payload: ResourceNodePayload,
    backend: Option<BackendBinding>,
    acl: NamespaceAcl,
    metadata: CommonResourceMetadata,
    created_at: Timestamp,
    updated_at: Timestamp,
}

enum ResourceNodeKind {
    Root,
    Runner,
    Device,
    RuntimeNetwork,
    RuntimeDnsRecord,
    Storage,
    AuthSession,
    AccessGrant,
    ConsentGrant,
    NetworkPolicy,
    Capsule,
    Instance,
    Revision,
    LaunchProfile,
    State,
    SecretRef,
    Session,
    Port,
    Log,
    Receipt,
    Artifact,
    Directory,
    Link,
}

enum ResourceNodePayload {
    None,
    Runner(RunnerNode),
    Device(DeviceNode),
    RuntimeNetwork(RuntimeNetworkNode),
    RuntimeDnsRecord(RuntimeDnsRecordNode),
    Storage(StorageNode),
    AuthSession(AuthSessionNode),
    AccessGrant(AccessGrantNode),
    ConsentGrant(ConsentGrantNode),
    NetworkPolicy(NetworkPolicyNode),
    Capsule(CapsuleNode),
    Instance(InstanceNode),
    Revision(RevisionNode),
    LaunchProfile(LaunchProfileNode),
    State(StateNode),
    SecretRef(SecretRefNode),
    Session(SessionNode),
    Port(PortNode),
    Log(LogNode),
    Receipt(ReceiptNode),
    Artifact(ArtifactNode),
    Link(ResourceLink),
}

enum ResourceLinkKind {
    Alias,
    BackReference,
    AttachedReference,
}

struct ResourceLink {
    from: ResourcePath,
    to: ResourceRef,
    kind: ResourceLinkKind,
}

struct BackendBinding {
    backend: ResourceBackend,
    backend_id: String,
    object_ref: String,
    integrity: Option<IntegrityRef>,
}

enum ResourceBackend {
    ControlPlaneStore,
    LocalDesktopState,
    ArtifactStore,
    RunnerEphemeral,
    SecretManager,
}

struct StateBinding {
    instance: ResourceRef,
    contract_name: String,
    storage: ResourceRef,
    path_prefix: String,
    read_only: bool,
    affects_execution_identity: bool,
}

struct SecretRef {
    scope: ResourceRef,
    secret_provider: String,
    ref_id: String,
    version: Option<String>,
    redaction: RedactionPolicy,
}

struct ArtifactRef {
    build_id: ArtifactBuildId,
    path: ResourcePath,
    content_hash: String,
    media_type: String,
}

struct ReceiptRef {
    path: ResourcePath,
    receipt_id: String,
    content_hash: Option<String>,
}

struct LaunchProfileNode {
    profile_id: String,
    runtime: RuntimeRequirement,
    runner_selector: RunnerSelector,
    device_selector: Option<DeviceSelector>,
    storage_bindings: Vec<StorageBindingRequirement>,
    secret_bindings: Vec<SecretRequirement>,
    network_policy: NetworkRequirement,
    requirement_graph: LaunchRequirementGraph,
}

struct RunnerSelector {
    allow_classes: Vec<RunnerClass>,
    deny_classes: Vec<RunnerClass>,
    allowed_runner_refs: Vec<ResourceRef>,
    denied_runner_refs: Vec<ResourceRef>,
    min_trust: RunnerTrustLevel,
    locality: Option<LocalityConstraint>,
}

struct ProjectionDigest {
    source: ResourceRef,
    projection_kind: String,
    digest: String,
}

struct LaunchMaterializationRecord {
    session: ResourceRef,
    profile: ResourceRef,
    requirement_graph: LaunchRequirementGraph,
    requirement_bindings: Vec<RequirementBinding>,
    input_refs: Vec<ResourceRef>,
    projection_digests: Vec<ProjectionDigest>,
    execution_id: String,
    materialized_at: Timestamp,
}

struct SessionRoute {
    session: ResourceRef,
    runner: ResourceRef,
    ports: Vec<PortBinding>,
    network_scope: NetworkScope,
}

struct SessionNode {
    instance: ResourceRef,
    runner: Option<ResourceRef>,
    lifecycle: SessionLifecycle,
    route: Option<SessionRoute>,
    execution_identity: Option<LaunchMaterializationRecord>,
}

struct SessionLifecycle {
    status: SessionStatus,
    started_at: Option<Timestamp>,
    stopped_at: Option<Timestamp>,
    last_observed_at: Option<Timestamp>,
    terminal_reason: Option<String>,
}

enum SessionStatus {
    Planned,
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
    Expired,
    Finalized,
}

struct PortBinding {
    name: String,
    protocol: PortProtocol,
    container_port: u16,
    runner_port: Option<u16>,
    local_port: Option<u16>,
    public_url: Option<String>,
}

struct NamespaceAcl {
    owner_subject: String,
    entries: Vec<NamespaceAclEntry>,
    runner_trust_required: Option<RunnerTrustLevel>,
}

struct NamespaceAclEntry {
    subject: String,
    operations: Vec<NamespaceOperation>,
    condition: Option<String>,
}

enum NamespaceOperation {
    Get,
    List,
    Resolve,
    Mount,
    Unmount,
    BindState,
    BindSecret,
    AttachSession,
    DetachSession,
    RecordReceipt,
    AppendLog,
    Admin,
}
```

Application requirement domain:

```rust
struct LaunchRequirementGraph {
    graph_id: String,
    nodes: Vec<RequirementNode>,
    edges: Vec<RequirementEdge>,
}

struct RequirementEdge {
    from: String,
    to: String,
    relation: RequirementRelation,
}

enum RequirementRelation {
    DependsOn,
    CoLocateWith,
    Consumes,
    Produces,
    Exposes,
    RequiresConsent,
    MustUseSameRunner,
    MustNotUseRunner,
    RequiresNetworkPolicy,
    RequiresSecretProjection,
}

enum RequirementNode {
    Runtime(RuntimeRequirement),
    Storage(StorageRequirement),
    Network(NetworkRequirement),
    Secret(SecretRequirement),
    Auth(AuthRequirement),
    Input(InputRequirement),
    Output(OutputRequirement),
    Device(DeviceRequirement),
    Service(ServiceRequirement),
    Policy(PolicyRequirement),
    Io(IoRequirement),
}

struct RequirementBinding {
    requirement_id: String,
    binding_kind: RequirementBindingKind,
    resolved_resource: Option<ResourceRef>,
    resolved_resources: Vec<ResourceRef>,
    enforcement: Option<EnforcementRef>,
    consent: Option<ResourceRef>,
    evidence: Vec<BindingEvidence>,
    affects_execution_identity: bool,
}

enum RequirementBindingKind {
    Resource,
    ResourceSet,
    RunnerCapability,
    EnforcementOnly,
    ConsentOnly,
    ProvisionedResource,
}

struct EnforcementRef {
    enforcement_kind: String,
    resource: Option<ResourceRef>,
    runner_ref: Option<ResourceRef>,
}

struct BindingEvidence {
    source: String,
    digest: Option<String>,
    observed_at: Timestamp,
}

struct RuntimeRequirement {
    requirement_id: String,
    kind: RuntimeRequirementKind,
    runtimes: Vec<RuntimeKind>,
    browser: Option<BrowserRuntimeRequirement>,
    server_process: bool,
    oci: bool,
    wasm: bool,
}

struct BrowserRuntimeRequirement {
    wasm: bool,
    web_worker: bool,
    service_worker: bool,
    opfs: bool,
    webgpu: bool,
}

struct StorageRequirement {
    requirement_id: String,
    name: String,
    kind: StorageRequirementKind,
    access: Vec<StorageAccess>,
    persistence: StoragePersistence,
}

struct StorageBindingRequirement {
    requirement_id: String,
    storage_requirement: StorageRequirement,
    path_prefix: String,
    read_only: bool,
}

struct NetworkRequirement {
    requirement_id: String,
    mode: NetworkRequirementMode,
    allow: Vec<String>,
    deny: Vec<String>,
}

struct SecretRequirement {
    requirement_id: String,
    name: String,
    kind: SecretRequirementKind,
    required: bool,
    runner_projection: SecretProjectionPolicy,
}

struct AuthRequirement {
    requirement_id: String,
    kind: AuthRequirementKind,
    scopes: Vec<String>,
}

struct InputRequirement {
    requirement_id: String,
    name: String,
    io: IoRequirement,
}

struct OutputRequirement {
    requirement_id: String,
    name: String,
    io: IoRequirement,
}

struct DeviceRequirement {
    requirement_id: String,
    device_selector: DeviceSelector,
}

struct ServiceRequirement {
    requirement_id: String,
    service_kind: String,
    required: bool,
}

struct PolicyRequirement {
    requirement_id: String,
    policy_kind: String,
    required: bool,
}

struct LaunchResolutionInput {
    subject: SubjectRef,
    auth_context: AuthContext,
    instance: ResourceRef,
    profile: ResourceRef,
    requirement_graph: LaunchRequirementGraph,
    namespace_snapshot: NamespaceSnapshot,
}

trait RequirementResolver {
    fn resolve(
        requirement: RequirementNode,
        context: ResolutionContext,
    ) -> ResolutionResult;
}

struct ResolutionResult {
    requirement_id: String,
    candidates: Vec<BindingCandidate>,
    consent_gaps: Vec<ConsentGap>,
    provisioning_actions: Vec<ProvisioningAction>,
    errors: Vec<ResolutionError>,
}

struct BindingCandidate {
    resources: Vec<ResourceRef>,
    constraints: DerivedConstraints,
    enforcement: Option<EnforcementPlan>,
    score_hint: Option<i64>,
    affects_execution_identity: bool,
}

struct BindingPlan {
    requirement_bindings: Vec<RequirementBinding>,
    derived_runner_constraints: RunnerConstraints,
    consent_gaps: Vec<ConsentGap>,
    provisioning_actions: Vec<ProvisioningAction>,
    identity_inputs: Vec<IdentityInput>,
}

struct LaunchResolutionPlan {
    plan_id: String,
    subject: SubjectRef,
    instance: ResourceRef,
    profile: ResourceRef,
    requirement_graph: LaunchRequirementGraph,
    binding_plan: BindingPlan,
    placement_candidates: Vec<PlacementCandidate>,
    selected_runner: Option<ResourceRef>,
    consent_gaps: Vec<ConsentGap>,
    provisioning_actions: Vec<ProvisioningAction>,
    rejection_reasons: Vec<ResolutionRejection>,
    plan_hash: String,
    created_at: Timestamp,
    expires_at: Timestamp,
}

struct ConsentGap {
    requirement_id: String,
    reason: String,
    requested_scope: String,
    candidate_resources: Vec<ResourceRef>,
}

struct ProvisioningAction {
    action_id: String,
    kind: ProvisioningActionKind,
    creates: Vec<ResourceRef>,
    requires_consent: bool,
}

enum ProvisioningActionKind {
    CreateManagedRunner,
    CreateBrowserRunner,
    CreateNetworkPolicy,
    CreateStorageBinding,
    CreateSecretRef,
    CreateAuthGrant,
}

struct InstallRevision {
    install_revision_id: String,
    install_profile_key: String,
    artifact_build_id: ArtifactBuildId,
    requirement_graph: RequirementGraphSnapshot,
    state_contracts: Vec<StateContractSnapshot>,
    launch_templates: Vec<LaunchTemplate>,
    compatibility_index: CompatibilityIndex,
    install_receipt: InstallReceipt,
    created_at: Timestamp,
}

struct ArtifactBuild {
    artifact_build_id: ArtifactBuildId,
    capsule_ref: ResourceRef,
    source_provenance: ResourceRef,
    output_ref: ArtifactRef,
    dependency_output_hash: Option<String>,
    build_receipt: ReceiptRef,
    created_at: Timestamp,
}

struct RequirementGraphSnapshot {
    snapshot_id: String,
    graph: LaunchRequirementGraph,
    graph_hash: String,
    source_revision: ResourceRef,
    profile_defaults_hash: String,
}

struct BindingAssignmentSet {
    binding_set_id: String,
    binding_set_hash: String,
    instance: ResourceRef,
    profile: ResourceRef,
    requirement_graph_hash: String,
    assignments: Vec<RequirementBinding>,
    created_from: BindingAssignmentSource,
}

struct LaunchTemplate {
    template_id: String,
    template_hash: String,
    key: LaunchTemplateKey,
    profile: ResourceRef,
    artifact: ArtifactRef,
    requirement_graph: ResourceRef,
    binding_assignment_set: ResourceRef,
    filesystem_view_template: FilesystemViewTemplate,
    network_policy_template: NetworkPolicyTemplate,
    capability_policy_template: CapabilityPolicyTemplate,
    runner_compatibility_class: RunnerCompatibilityClass,
}

struct LaunchTemplateKey {
    install_revision_id: String,
    profile_hash: String,
    requirement_graph_hash: String,
    binding_set_hash: String,
    network_policy_hash: String,
    capability_policy_hash: String,
    state_contract_hash: String,
    runner_compatibility_class: RunnerCompatibilityClass,
}

struct CompatibilityIndex {
    index_id: String,
    supported_runner_classes: Vec<RunnerClass>,
    denied_runner_classes: Vec<RunnerClass>,
    required_capabilities: Vec<String>,
    optional_capabilities: Vec<String>,
    precheck_hash: String,
}

struct ConsentSnapshot {
    snapshot_id: String,
    instance: ResourceRef,
    profile: Option<ResourceRef>,
    grants: Vec<ResourceRef>,
    snapshot_hash: String,
    captured_at: Timestamp,
}

struct StateContractSnapshot {
    contract_name: String,
    storage_requirement: StorageRequirement,
    expected_shape_hash: String,
    state_contract_hash: String,
}

struct InstallReceipt {
    receipt_id: String,
    install_profile_key: String,
    install_revision_id: String,
    artifact_build_id: ArtifactBuildId,
    resolved_inputs: Vec<ResourceRef>,
    output_hashes: Vec<String>,
    occurred_at: Timestamp,
}
```

Runner domain:

```rust
struct RunnerNode {
    runner_id: String,
    metadata: RunnerMetadata,
    enrollment: Option<RunnerEnrollment>,
    health: Option<RunnerHealth>,
    latest_capability_snapshot: Option<ResourceRef>,
}

struct RunnerMetadata {
    schema: String, // "ato.runner.metadata.v1"
    runner_id: String,
    owner_subject: String,
    runner_class: RunnerClass,
    display_name: String,
    device_ref: Option<ResourceRef>,
    runtime_network_ref: Option<ResourceRef>,
    control_surfaces: RunnerControlSurfaces,
    access_policy: RunnerAccessPolicy,
    placement: RunnerPlacementMetadata,
    transport: RunnerTransportMetadata,
    sandbox: Option<BrowserRunnerSandbox>,
    trust_level: RunnerTrustLevel,
    labels: Map<String, String>,
}

enum RunnerClass {
    ManagedRunner,
    DesktopRunner,
    ExternalRunner,
    BrowserRunner,
    BrowserPreviewRunner,
}

struct RunnerControlSurfaces {
    allowed: Vec<ControlSurface>,
    default: ControlSurface,
    runner_visibility: Option<RunnerVisibility>,
}

struct RunnerAccessPolicy {
    direct_local_ui: AccessMode,
    desktop_ui: AccessMode,
    api_access: ApiAccessMode,
}

enum RunnerPlacementMetadata {
    Managed { provider: String, region: String },
    Desktop { machine_id: String },
    External { provider: String, runner_id: String },
    Browser { execution_model: BrowserExecutionModel },
}

enum RunnerTransportMetadata {
    PublicRoute { base_domain: String },
    AtoNet { dns_name: String, tunnel: String },
    RunnerPullQueue,
    BrowserSession { session_binding: String },
}

struct BrowserRunnerSandbox {
    iframe: bool,
    web_worker: bool,
    service_worker: bool,
    opfs: bool,
    network_policy: BrowserNetworkPolicy,
}

struct DeviceNode {
    device_id: String,
    owner_subject: String,
    display_name: String,
    device_class: DeviceClass,
    can_execute: bool,
    runtime_network_ref: Option<ResourceRef>,
}

struct RuntimeNetworkNode {
    runtime_network_id: String,
    owner_subject: String,
    name: String,
    trust_domain: String,
}

struct RuntimeDnsRecordNode {
    runtime_network: ResourceRef,
    name: String,
    fqdn: String,
    target_kind: RuntimeDnsTargetKind,
    target_ref: ResourceRef,
    record_type: RuntimeDnsRecordType,
    address: Option<String>,
    port: Option<u16>,
    ttl_seconds: u32,
    status: RuntimeDnsRecordStatus,
    last_verified_at: Option<Timestamp>,
    expires_at: Option<Timestamp>,
}

struct StorageNode {
    storage_id: String,
    owner_subject: String,
    storage_class: StorageClass,
    metadata: StorageMetadata,
    credential_ref: Option<ResourceRef>,
    capabilities: StorageCapabilities,
}

enum StorageClass {
    ObjectStore,
    S3Compatible,
    R2,
    LocalDirectory,
    PersistentVolume,
    EphemeralVolume,
    Database,
    KvStore,
    Cache,
}

struct StorageMetadata {
    endpoint: Option<String>,
    bucket: Option<String>,
    region: Option<String>,
    namespace_prefix: Option<String>,
}

struct StorageCapabilities {
    read: bool,
    write: bool,
    list: bool,
    delete: bool,
    signed_url: bool,
    consistency: StorageConsistency,
    max_object_size_bytes: Option<u64>,
}

struct AuthContext {
    subject: SubjectRef,
    device: Option<ResourceRef>,
    runner: Option<ResourceRef>,
    auth_session: Option<ResourceRef>,
    grants: Vec<ResourceRef>,
}

struct AuthSessionNode {
    auth_session_id: String,
    subject: SubjectRef,
    device: Option<ResourceRef>,
    expires_at: Option<Timestamp>,
}

struct AccessGrantNode {
    grant_id: String,
    subject: SubjectRef,
    scope: ResourceRef,
    operations: Vec<NamespaceOperation>,
    expires_at: Option<Timestamp>,
    issued_by: String,
}

struct ConsentGrantNode {
    consent_id: String,
    instance: ResourceRef,
    profile: Option<ResourceRef>,
    subject: SubjectRef,
    grants: Vec<RequirementBinding>,
    expires_at: Option<Timestamp>,
}

struct NetworkPolicyNode {
    network_policy_id: String,
    owner_subject: String,
    requirement: NetworkRequirement,
    enforcement: NetworkPolicyEnforcement,
}

struct IoRequirement {
    class: IoResourceClass,
    access: Vec<IoAccess>,
    required: bool,
}

struct IoCapability {
    runner: ResourceRef,
    class: IoResourceClass,
    available: bool,
    mediated_by: IoMediator,
}

enum IoResourceClass {
    Display,
    AudioOutput,
    Microphone,
    Camera,
    Keyboard,
    Pointer,
    Clipboard,
    FilePicker,
    DirectoryHandle,
    Gpu,
    WebGpu,
    WebUsb,
    WebSerial,
    Gamepad,
    NetworkInterface,
}

enum IoMediator {
    BrowserApi,
    DesktopOs,
    ContainerRuntime,
    ManagedCloud,
    ExternalRunner,
}

struct RunnerEnrollment {
    runner_ref: ResourceRef,
    enrollment_id: String,
    token_hash: String,
    token_expires_at: Timestamp,
    status: RunnerEnrollmentStatus,
    claimed_at: Option<Timestamp>,
    claimed_by: Option<String>,
    revoked_at: Option<Timestamp>,
    trust_level: RunnerTrustLevel,
}

enum RunnerEnrollmentStatus {
    Issued,
    Claimed,
    Active,
    Revoked,
    Expired,
}

struct RunnerCapabilitySnapshot {
    runner_ref: ResourceRef,
    snapshot_id: String,
    runtimes: Vec<RuntimeKind>,
    cpu_cores: u32,
    memory_bytes: u64,
    gpus: Vec<GpuCapability>,
    regions: Vec<String>,
    network: RunnerNetworkCapability,
    redacted: bool,
    observed_at: Timestamp,
}

struct RunnerHealth {
    runner_ref: ResourceRef,
    status: RunnerStatus,
    last_heartbeat_at: Timestamp,
    active_sessions: u32,
    taints: Vec<RunnerTaint>,
}

struct RunnerCommandQueue {
    runner_ref: ResourceRef,
    queue_ref: ResourceRef,
    authorization: CommandQueueAuthorization,
}

struct RunnerCommand {
    command_id: String,
    idempotency_key: String,
    runner_ref: ResourceRef,
    payload: RunnerCommandPayload,
    status: RunnerCommandStatus,
    claimed_by: Option<String>,
    lease_until: Option<Timestamp>,
    attempt: u32,
    issued_at: Timestamp,
    expires_at: Timestamp,
    signature: Option<String>,
}

enum RunnerCommandStatus {
    Queued,
    Claimed,
    Running,
    Succeeded,
    Failed,
    Expired,
    Cancelled,
}

enum RunnerCommandPayload {
    PrepareSession { session: ResourceRef, materialization_plan: ResourceRef },
    StartSession { session: ResourceRef, launch_envelope_ref: String },
    StopSession { session: ResourceRef, reason: String },
    RoutePort { session: ResourceRef, port: PortBinding },
    CollectLogs { session: ResourceRef, cursor: Option<String> },
    CollectReceipt { session: ResourceRef },
    SnapshotState { instance: ResourceRef, state: ResourceRef },
    DrainRunner { reason: String },
}

struct RunnerCommandResult {
    command_id: String,
    status: RunnerCommandStatus,
    session: Option<ResourceRef>,
    logs: Vec<ResourceRef>,
    receipt: Option<ResourceRef>,
    error: Option<RunnerCommandError>,
    completed_at: Timestamp,
}
```

Placement domain:

```rust
struct ResourceConstraints {
    required_runtimes: Vec<RuntimeKind>,
    min_cpu_cores: Option<u32>,
    min_memory_bytes: Option<u64>,
    gpu: Option<GpuConstraints>,
    locality: Option<LocalityConstraint>,
    network: Option<NetworkConstraint>,
    consent_scope: ResourceRef,
}

struct PlacementCandidate {
    runner: ResourceRef,
    capability_snapshot: RunnerCapabilitySnapshot,
    health: RunnerHealth,
    score: Option<i64>,
    rejection_reasons: Vec<PlacementReason>,
}

struct PlacementDecision {
    decision_id: String,
    constraints: ResourceConstraints,
    candidates: Vec<PlacementCandidate>,
    selected_runner: Option<ResourceRef>,
    selected_session: Option<ResourceRef>,
    reasons: Vec<PlacementReason>,
    decided_at: Timestamp,
}

enum PlacementReason {
    Satisfied(String),
    Rejected(String),
    InsufficientCpu,
    InsufficientMemory,
    InsufficientGpu,
    RuntimeUnsupported,
    LocalityMismatch,
    RunnerUnhealthy,
    ConsentMissing,
    FallbackFirstFit,
}
```

Artifact / Audit domain:

```rust
struct ArtifactBuildId(String); // domain form: "blake3:<hex>", path form: "/artifacts/blake3/<hex>"

struct CapsuleArtifact {
    build_id: ArtifactBuildId,
    capsule_ref: ResourceRef,
    media_type: String,
    content_hash: String,
    signature: Option<SyncSignatureShape>,
    created_at: Timestamp,
}

struct ExecutionReceiptRef {
    session: ResourceRef,
    receipt: ReceiptRef,
    placement_decision: Option<String>,
}

struct AuditEvent {
    event_id: String,
    owner_subject: String,
    chain_scope: AuditChainScope,
    operation: NamespaceOperation,
    actor: String,
    path: ResourcePath,
    result: AuditResult,
    prev_hash: Option<String>,
    event_hash: String,
    hmac: Option<String>,
    occurred_at: Timestamp,
}

enum AuditChainScope {
    OwnerSubject,
    ResourcePath,
}

struct UsageMeterEvent {
    event_id: String,
    subject: ResourceRef,
    session: Option<ResourceRef>,
    meter: String,
    quantity: Decimal,
    observed_at: Timestamp,
}
```

## Relationship to Application Requirement Graph

Resource Namespace は application requirement graph そのものではない。Application Requirement Graph はアプリが要求する外部条件を表し、Resource Namespace は control plane が管理する実体への参照を表し、`LaunchMaterializationRecord` は requirement がどの resource に解決されたかを記録する。

```text
Application Requirement Graph
  runtime, storage, network, auth, secret, input, output, gpu, browser capability

Ato Resource Namespace
  /runners/run_x, /storage/stor_x, /secrets/sec_x, /auth/grants/grant_x,
  /network_policies/netpol_x, /sessions/ses_x

Materialization Record
  requirement "storage.user_data" -> /storage/stor_x
  requirement "runtime.browser" -> /runners/run_browser_x
```

アプリ側の宣言には `/runners`、`/storage/stor_x`、`/sessions` のような Ato control plane path を入れない。アプリは「WASM が必要」「S3互換 storage が必要」「camera input が必要」「proxy-only network が必要」のように要求を宣言し、Ato がそれを Resource Namespace 上の resource に解決する。

例:

```toml
[requires.runtime]
kind = "browser"
wasm = true
web_worker = true

[requires.network]
mode = "proxy_only"
allow = ["https://api.example.com"]

[requires.storage.user_data]
kind = "object_store"
access = ["read", "write"]
persistence = "durable"

[requires.auth.github]
kind = "oauth"
scopes = ["repo:read"]
```

materialization:

```text
requires.runtime
  -> /runners/run_browser_...
requires.storage.user_data
  -> /storage/stor_...
requires.auth.github
  -> /auth/grants/grant_...
requires.network
  -> /network_policies/netpol_...
```

`RequirementBinding.affects_execution_identity` は、binding が launch envelope identity に入るかどうかを明示する。diagnostic observation は requirement binding ではないため、execution drift と混同しない。

`RequirementBinding.resolved_resource` は単一 resource に解決できる場合だけ使う。camera input、browser permission、proxy-only network、secret projection のように複数 resource と enforcement の組で満たされる requirement は、`resolved_resources`、`enforcement`、`consent`、`evidence` を使って表す。`EnforcementRef`、`BindingEvidence`、`NamespaceSnapshot`、`ResolutionContext`、`DerivedConstraints`、`RunnerConstraints`、`IdentityInput`、`ResolutionError`、`ResolutionRejection` は実装言語ごとに定義される RFC-level domain type であり、stringly typed payload にしてはならない。

## Requirement Resolution Algorithm

Launch は runner selection を一発で行う処理ではない。v0 の launch resolution は、Application Requirement Graph を Resource Namespace 上の binding plan に変換し、その後 runner placement を行い、最後に materialization を transaction として commit する staged deterministic resolver とする。

v0 の段階:

1. Compile: capsule、store listing、install revision、launch profile から `LaunchRequirementGraph` を作る。
2. Resolve: requirement を namespace resource、provisioning action、consent gap、derived runner constraints に解決する。
3. Place: binding plan と runner constraints から runner を Filter→Score→Select で選ぶ。
4. Materialize: session、network policy、consent、placement decision、launch materialization record、runner command に落とす。

Step 0: Inputs

`LaunchResolutionInput` は subject、auth context、instance、profile、requirement graph、namespace snapshot を入力にする。resolver は live DB を都度読むのではなく、resolution 開始時点の snapshot と明示的な revalidation step を通す。

Step 1: profile overlay

同じ capsule でも profile によって runner、storage、secret、network、I/O が変わる。まず base requirements に `LaunchProfileNode.runner_selector`、`device_selector`、`storage_bindings`、`secret_bindings`、`network_policy`、user/workspace defaults を overlay し、effective `LaunchRequirementGraph` を作る。profile が `browser_runner` only なのに `server_process = true` を要求する場合、この段階で `ResolutionRejection` にできる。

Step 2: graph validation

ここでは resource を選ばず、要求自体の矛盾を検出する。

- `runtime.kind = browser` かつ secret runner projection required は v0 では invalid、または managed/desktop fallback が必要である。
- `network.mode = proxy_only` と `runtime.kind = oci` は許可してよいが、enforcement backend が browser proxy ではなく runner network policy になる。
- `storage.kind = object_store`、write access、ephemeral persistence の組み合わせは、明示的な temporary object store がない限り矛盾として扱う。
- `DeviceRequirement` が stale device だけを許す場合、runner filtering 前に candidate deficiency として記録する。

Step 3: topological ordering

`RequirementEdge` に基づき、requirement を解く順序を決める。default order は Runtime、Storage、Auth、Secret、Network、I/O、Runner/Device、Policy とする。ただし `DependsOn`、`RequiresSecretProjection`、`RequiresNetworkPolicy`、`MustUseSameRunner` の edge がある場合は edge を優先する。

例:

- storage credential は secret requirement に `DependsOn` する。
- proxy-only network は runner backend の enforcement に `RequiresNetworkPolicy` する。
- camera input は browser runner capability と browser permission consent に `RequiresConsent` する。
- storage endpoint access と secret projection は、選ばれる runner class によって可否が変わるため `MustUseSameRunner` constraint を導出しうる。

Step 4: resolver registry

各 `RequirementNode` kind は `RequirementResolver` を持つ。resolver は runner を即決せず、その requirement を満たす candidate resource、provisioning action、consent gap、derived runner constraints、enforcement plan を返す。resolver は namespace を変更してはならない。

例:

- browser + WASM runtime requirement は `runner_class includes browser_runner`、`wasm = true`、`web_worker = true` の runner constraint を出す。
- S3-compatible storage write requirement は `/storage/<storage_id>` candidate、credential secret ref、S3 endpoint への runner egress constraint を出す。
- proxy-only network requirement は `/network_policies/<network_policy_id>` candidate または `CreateNetworkPolicy` provisioning action と `AtoProxyOnly` enforcement plan を出す。
- secret requirement は `/secrets/<secret_ref_id>` candidate と runner class ごとの projection policy を出し、Browser Runner secret injection は v0 で reject する。

Step 5: construct binding plans

各 requirement の `BindingCandidate` を組み合わせ、矛盾しない `BindingPlan` を作る。v0 は SAT solver / ILP を使わず、次の優先順位で deterministic に候補を選ぶ。

1. explicit profile binding
2. existing user resource
3. workspace default resource
4. managed Ato default
5. provision new resource

`BindingPlan` は requirement bindings、derived runner constraints、consent gaps、provisioning actions、execution identity に入る `IdentityInput` を持つ。候補探索で namespace node を作ってはならない。

Step 6: runner filtering

`BindingPlan` ごとに runner candidate を列挙する。候補は runtime network 内の active runners を起点に、owner subject、`RunnerSelector`、runner health、trust level、capability snapshot、derived runner constraints で filter する。

filter examples:

- Browser Runner は `runtime.kind = browser` なら候補になりうるが、server process、secret projection required、proxy-only enforcement 不可、required Web feature 不足では reject する。
- Managed Runner は OCI/server process に適するが、profile が desktop-only の場合や storage endpoint へ到達できない場合は reject する。
- Desktop Runner は user-owned trust と active device が条件であり、stale device や managed-only profile では reject する。

Step 7: score

hard filter は correctness、security、consent、trust、capability を扱う。soft score は profile preference、capability match quality、locality、cost、expected startup latency、existing warm session/cache、runner health freshness を扱う。v0 の score は単純でよいが、`PlacementReason` に filter/reject/score の理由を残す。

Step 8: consent checkpoint

consent は placement 前にも後にも出る。S3 storage、GitHub OAuth、camera、secret projection は placement 前に分かる。選ばれた runner class、managed region、browser capability、secret projection target は placement 後に確定する。

`LaunchResolutionPlan` に `plan_id`、`plan_hash`、`expires_at`、`consent_gaps` を保存し、consent が足りない場合は `ConsentRequired(plan_id, gaps)` を返す。ユーザー承認後は同じ plan をそのまま commit せず、runner health、capability snapshot、resource ACL、secret version、network policy を revalidate する。revalidation に失敗した plan は expired または rejected にする。

Step 9: commit materialization transaction

selected plan が revalidated されたら、初めて namespace に永続 node を作る。commit は transaction として扱い、部分成功で session だけ残してはならない。

作るもの:

- `/network_policies/<network_policy_id>`
- `/sessions/<session_id>`
- `/instances/<install_profile_key>/sessions/<session_id>` の `BackReference`
- `/consents/<consent_id>` if new
- `LaunchMaterializationRecord`
- `PlacementDecision`
- `AuditEvent`

Step 10: runner prepare / start

runner には `StartSession` をいきなり送らない。v0 は `PrepareSession` と `StartSession` を分ける。

1. `RunnerCommandPayload::PrepareSession { session, materialization_plan }` を queue に入れる。
2. runner は artifact projection、storage binding projection、network policy projection、secret projection、launch envelope assembly readiness を返す。
3. control plane は projection digest と readiness を `LaunchMaterializationRecord.projection_digests` に保存し、`execution_id` を確定する。
4. `RunnerCommandPayload::StartSession { session, launch_envelope_ref }` を queue に入れる。

これにより、requirement resolution、runner placement、launch envelope identity、runner execution の責務を分離したまま、receipt diff と observed drift を追跡できる。

## Install Outputs and Launch Reuse

Install は bindings 設定ファイルをそのまま保存する処理ではない。Install は起動可能な output-first revision と、launch 時の重い再解決を避けるための中間生成物を作る。Launch はそれらを再利用しつつ、runner health、capability、auth、consent、secret、storage、network enforcement、state lock などの volatile 条件を毎回 revalidate する。

identity separation:

- `install_profile_key` はユーザーが shortcut / dashboard / mobile control surface から安定参照する installed app/profile key である。
- `install_revision_id` は immutable installed output revision であり、build output と requirement snapshot に対応する。
- `artifact_build_id` は build artifact cache identity であり、session state、dynamic port、process id、route、log cursor を含めない。
- `execution_id` は launch envelope identity であり、filesystem view、network policy、capability policy、entrypoint、argv、cwd、state/secret/artifact/runner projection の digest に影響されうる。
- `capsule_instance_key` は exact replay/session key であり、`install_profile_key + install_revision_id + execution_id` から導出する。

Install が作るべき object:

| Object | 役割 | Session 固有か |
|---|---|---|
| `InstallRevision` | immutable な installed output revision。artifact、requirement graph、state contract、launch template を束ねる。 | No |
| `ArtifactBuild` | build output / dependency output / source materialization の cache identity。 | No |
| `RequirementGraphSnapshot` | capsule + profile defaults から compile した application requirement graph。 | No |
| `BindingAssignmentSet` | desired bindings を normalize / resolve した requirement_id -> resource / selector / policy の集合。 | No |
| `LaunchTemplate` | profile + binding + policy から作る session 非依存の launch envelope 雛形。 | No |
| `CompatibilityIndex` | どの runner class / capability set なら起動可能かの precheck。 | No |
| `ConsentSnapshot` | instance-scoped consent の install/revision 時点 snapshot。launch では必ず再検証する。 | No |
| `StateContractSnapshot` | state/storage の期待形と hash。 | No |
| `InstallReceipt` | install 時点で何を解決・生成したかの監査 record。 | No |
| `LaunchMaterializationRecord` | 特定 session / runner に実際に投影した記録。 | Yes |

Desktop/local の論理 layout example:

```text
instances/<install_profile_key>/
  instance.json
  profiles/
    default.jsonc
    debug.jsonc
  bindings/
    desired.jsonc
    resolved.<binding_set_hash>.json
  consents/
    consents.json
  state/
  sessions/
revisions/<install_revision_id>/
  revision.json
  artifact-manifest.json
  source-provenance.json
  requirement-graph.json
  state-contracts.json
  launch-template.<profile_hash>.json
  compatibility-index.json
  install-receipt.json
artifact-cache/<artifact_build_id>/
  output/
  dependency-output/
  build-receipt.json
```

Cloud / Web Console では同じ model を DB と object store に分ける。DB は installed apps、install profiles、install revisions、artifact builds、requirement graph snapshots、binding assignment sets、launch templates、compatibility indexes、consent grants、state contracts を持つ。object store は build outputs、source snapshots、large receipts、logs、archive bundles を持つ。

`bindings/desired.jsonc` は user desired input であり、install の成果物の正本ではない。正本は normalize / resolve 後の `BindingAssignmentSet` と `LaunchTemplate` である。Launch は desired bindings を毎回解釈し直さず、`BindingAssignmentSet` と `LaunchTemplate` を読み、volatile 条件だけを revalidate する。

Install 時に固定できるもの:

- source tree hash
- dependency derivation hash
- dependency output hash
- `artifact_build_id`
- `install_revision_id`
- compiled `RequirementGraphSnapshot`
- normalized launch profile defaults
- `StateContractSnapshot`
- storage binding assignment
- secret ref assignment
- network policy template
- filesystem view template
- capability policy template
- runner compatibility precheck

Launch 時に必ず revalidate するもの:

- runner が active か。
- runner capability snapshot が requirement と compatibility index をまだ満たすか。
- auth grant / consent が revoke されていないか。
- secret ref の version / availability。
- storage credential の有効性。
- Browser Runner の実測 capability。
- network policy enforcement backend。
- dynamic port / route。
- session id。
- state lock / concurrency policy。

Launch reuse algorithm:

1. `install_profile_key` を読む。
2. current `install_revision_id` を読む。
3. `profile_hash` を計算する。
4. current `binding_set_hash` を読む。
5. `RunnerSelector` と `CompatibilityIndex` を読む。
6. candidate runner の health/capability を revalidate する。
7. consent / auth / secret refs / storage credentials を revalidate する。
8. `LaunchTemplateKey` が一致し、invalidated されていなければ cached `LaunchTemplate` を使う。
9. session-specific fields を追加する。
10. `execution_id` を計算する。
11. `capsule_instance_key` を作る。
12. `PrepareSession` / `StartSession` に進む。

`LaunchTemplateKey` に入れるもの:

- `install_revision_id`
- `profile_hash`
- `requirement_graph_hash`
- `binding_set_hash`
- `network_policy_hash`
- `capability_policy_hash`
- `state_contract_hash`
- `runner_compatibility_class`

`LaunchTemplateKey` に入れてはいけないもの:

- `session_id`
- dynamic port
- process id
- container id
- live route
- log cursor
- observed status
- timestamp
- secret value

Cache invalidation rules:

- `install_revision_id` が変わったら、旧 revision の `LaunchTemplate` は current launch では使わない。
- profile defaults、requirement graph、binding assignment、network policy template、capability policy、state contract が変わったら `LaunchTemplateKey` を変える。
- consent revoke、auth grant revoke、secret version unavailability、storage credential expiration、runner capability downgrade は cached template を破棄するのではなく launch revalidation failure として扱う。ただし repeated failure は compatibility index refresh を要求してよい。
- runner health、dynamic port、route、observed readiness、log cursor、receipt facts は template invalidation input にしない。
- secret value は cache key、path、template payload に入れない。secret projection digest は `LaunchMaterializationRecord` にだけ保存する。

Three-layer separation:

```text
InstallRevision
  immutable build/output/revision identity
  launch ごとに変わらない

LaunchTemplate
  profile + bindings + policy から作る起動雛形
  runner class 依存を含んでよい
  session ごとに再利用できる

LaunchMaterializationRecord
  特定 session / runner に実際に投影した記録
  session ごとに凍結し、再利用しない
```

Example: `pgweb`

Install revision:

```json
{
  "install_revision_id": "irev_pgweb_001",
  "artifact_build_id": "abld_...",
  "requirement_graph_hash": "blake3:...",
  "state_contract_hash": "blake3:...",
  "supported_runner_classes": ["managed_runner", "desktop_runner"],
  "denied_runner_classes": ["browser_runner"]
}
```

Binding assignment:

```json
{
  "requirement_id": "secret.database_url",
  "resolved_resource": "/secrets/sec_database_url",
  "projection_policy": "env",
  "allowed_runner_classes": ["managed_runner", "desktop_runner"]
}
```

Launch は browser runner を compatibility index で即 reject し、managed/desktop runner の health と capability だけを再検証する。database URL の secret ref と consent を確認し、cached `LaunchTemplate` から runner-specific projection digest を作り、`execution_id` を計算する。source checkout、dependency install、OCI graph resolution、requirement graph compile はやり直さない。

Example: HTML + WASM Browser app

Install は `artifact_build_id`、`requirement_graph_hash`、browser compatibility index、proxy-only network template、CSP template を作る。Launch は current browser runner capability を probe し、`wasm = true`、`web_worker = true`、proxy/CSP enforcement availability、current auth session だけを再検証する。artifact hash、entrypoint、static asset map、network allowlist normalization、requirement graph は再計算しない。

## Relationship to Storage and State

`StateBinding` と `StorageNode` は別概念である。state はアプリ単位の意味論であり、storage は external resource の抽象である。

- `StorageNode` は S3互換 storage、R2、object store、local directory、persistent volume、database、KV/cache などを表す。
- `StateBinding` は instance の state contract を `/storage/<storage_id>` と `path_prefix` に binding する。
- Storage credential は `StorageNode.credential_ref` から `/secrets/<secret_ref_id>` を参照し、secret value は namespace node に保存しない。
- 同じ app でも profile ごとに local storage、R2、S3互換 storage、ephemeral volume を切り替えられる。

S3互換 storage の materialized shape:

```json
{
  "storage_class": "s3_compatible",
  "endpoint": "https://s3.example.com",
  "bucket": "my-app-data",
  "region": "auto",
  "credential_ref": "/secrets/sec_s3_...",
  "capabilities": {
    "read": true,
    "write": true,
    "list": true,
    "delete": false
  }
}
```

## Relationship to I/O Capabilities

Ato は OS device driver を実装しない。Ato が扱うのは、アプリが要求する I/O capability、ユーザーが許可した consent、runner が報告する I/O capability、起動時の binding、receipt に残す観測情報である。

Ato が扱うもの:

- アプリが要求する I/O capability。
- ユーザーが許可した I/O consent。
- runner が報告する I/O capability。
- 起動時にどの I/O が binding されたか。
- receipt に残す requested / allowed / denied / observed facts。

Ato が扱わないもの:

- OS の device driver 実装。
- kernel-level syscall mediation。
- GPU / camera / microphone / filesystem picker の低レベル実装。
- USB / serial / HID の直接 driver。

Browser Runner では camera、microphone、clipboard、file picker などは browser permission API によって mediated される。Desktop Runner では OS permission と runner policy が mediated する。Managed Runner では原則として physical I/O はなく、virtual I/O と network/storage binding だけを扱う。

## ResourcePath Rules

- path は必ず absolute であり、`/` から始まる。
- root path は `/` のみを許可する。
- `.` と `..` は禁止する。正規化時に除去せず、入力として検出した時点で拒否する。
- empty segment は禁止する。`//runners`、`/runners/` のような曖昧な表現は canonical path として保存しない。
- path comparison は `ResourcePath.canonical` だけで行う。
- v0 の path segment は `[A-Za-z0-9._~-]` のみを許可し、`:` は許可しない。表現できない外部 ID は ResourcePath 境界で percent encoding ではなく Ato canonical id に変換する。
- user-provided string と canonical path は型として分ける。外部入力は `ResourcePath::parse_user_input` のような境界を通す。
- OS path と ResourcePath を混同しない。`/instances/foo/state` は filesystem path ではない。
- Windows path / POSIX path / URL path と ResourcePath は別物である。`C:\...`、`file:///...`、`https://...` は ResourcePath ではない。
- secret value は path に含めない。secret manager の opaque ref も、漏えい可能な token を segment にしない。
- `execution_id`、`artifact_build_id`、`install_profile_key` などを path segment に入れる場合、文字種、長さ、prefix、case sensitivity を type ごとに定義する。
- `ArtifactBuildId` は domain form を `blake3:<hex>`、ResourcePath form を `/artifacts/blake3/<hex>` とする。hash algorithm と hex digest は別 segment に分ける。
- `install_profile_key` は shortcut / dashboard / mobile control surface から長期参照されるため、rename ではなく新 node 作成と alias metadata で扱う。
- `session_id` は session path の identity であり、runner-local process id ではない。
- `install_profile_key`、`session_id`、`secret_ref_id`、`receipt_id`、`log_id`、`storage_id`、`grant_id`、`consent_id`、`network_policy_id` は v0 では control plane global unique でなければならない。
- path segment は display name ではない。UI 表示名は node metadata に置く。

## Namespace Operations

v0 の operation は最小限に絞る。すべて operation-level authorization、path validation、audit event append を通る。

| Operation | 入力 | 出力 | 失敗条件 | ACL チェック | 監査イベント |
|---|---|---|---|---|---|
| `get(path)` | `ResourcePath` | `ResourceNode` | path 不正、node 不在、権限なし | `Get` | 成否を `AuditEvent` に記録 |
| `list(path)` | directory `ResourcePath` | child `ResourceRef[]` | path 不正、directory でない、権限なし | `List` | child count と cursor を記録 |
| `resolve(path)` | `ResourcePath` | backend/link 解決後の canonical node | mount 解決不能、link loop、backend 不整合 | `Resolve` | backend kind、link kind、result を記録 |
| `mount(path, backend)` | mount point、`BackendBinding` | `ResourceNode` | 既存 node 衝突、backend 不正、権限なし | `Mount` | backend kind と actor を記録 |
| `unmount(path)` | mount point | detached node ref | active session が依存、node 不在、権限なし | `Unmount` | detached path と reason を記録 |
| `bind_state(instance, state_contract)` | instance ref、state contract、backend | `StateBinding` | instance 不在、contract 不正、backend 不許可 | `BindState` | execution identity 影響有無を記録 |
| `bind_secret(instance, secret_ref)` | instance ref、`SecretRef` | `SecretRefNode` | secret value 混入、scope 不一致、consent 不足 | `BindSecret` | secret value なしで ref metadata のみ記録 |
| `attach_session(instance, session)` | instance ref、session node | `SessionNode` | duplicate session、runner 不在、placement 不整合 | `AttachSession` | runner ref と placement decision を記録 |
| `detach_session(session)` | session ref、reason | detached session state | session 不在、cleanup 未完了、権限なし | `DetachSession` | route cleanup / port release status を記録 |
| `record_receipt(session, receipt_ref)` | session ref、receipt ref | `ReceiptNode` | receipt hash 不一致、session 不在、権限なし | `RecordReceipt` | receipt hash と signature status を記録 |
| `append_log(session, log_ref)` | session ref、log ref、cursor | `LogNode` | cursor 逆行、log store 不可、権限なし | `AppendLog` | log stream、cursor、hash を記録 |

`mount` は v0 では control plane 内部の namespace mount であり、FUSE / 9P / WebDAV mount を意味しない。

`resolve` は `ResourceLink` を解決できるが、link loop を検出して拒否する。`get` は要求 path の node を返し、必要に応じて link payload を含める。canonical node が必要な caller は `resolve` を明示的に使う。

## Relationship to Execution Identity

Resource Namespace は「どこに何があるか」を表す。Execution Identity は「ある session がどの launch envelope で起動されたか」を表す。

`execution_id` は `/sessions/<session_id>/execution_identity` から参照される。namespace path の変更が必ず `execution_id` を変えるわけではない。たとえば log node の cursor 更新、receipt の追記、health metadata の更新は通常 `execution_id` を変えない。

launch envelope に入る `RequirementBinding`、`StateBinding`、`SecretRef`、`ArtifactRef`、runner projection は `execution_id` に影響しうる。したがって、`/instances/<install_profile_key>/profiles/<profile_id>` から launch envelope を materialize するときは、どの requirement と namespace node が identity input になったかを明示する。

v0 は `/sessions/<session_id>/execution_identity` の payload として `LaunchMaterializationRecord` を保存する。これは profile、requirement graph、requirement bindings、input refs、projection digests、execution id を結び、Resource Namespace と Execution Identity の責務を分けたまま receipt diff / observed drift と接続するための record である。

observed receipt は namespace 上の session/log/receipt node と接続するが、diagnostic facts は execution drift と混同しない。診断のために収集した runner health、log cursor、route observation は audit/receipt に残してよいが、launch envelope の identity input として扱うかは別に判定する。

## Relationship to PlacementGraph

PlacementGraph は Requirement Resolution pipeline の Place 段階である。PlacementGraph は non-runner resource binding を作る責務を持たず、`BindingPlan`、`RunnerSelector`、`RunnerCapabilitySnapshot`、derived `ResourceConstraints` を入力に runner placement decision を出す。Resource Namespace は runner candidates と chosen runner/session を stable path で指す。

`/runners/<runner_id>` は placement input である。`/sessions/.../runner` は placement output である。placement decision は audit / receipt に保存され、`/sessions/<session_id>/receipt` から到達できる。

PlacementGraph は `/runners/managed/fly/nrt` のような path prefix ではなく、`RunnerMetadata`、`RunnerCapabilitySnapshot`、`RunnerSelector`、requirement bindings、derived runner constraints を見て候補抽出する。

ApplicationRequirementGraph から runner を直接選んではならない。正しい流れは `ApplicationRequirementGraph -> Requirement Resolution -> BindingPlan -> Runner Placement -> LaunchMaterializationRecord -> Execution Identity` である。

v0 では first-fit selection を fallback として許可してよい。ただし標準の design reference は `onescluster/capsuled/client/pkg/scheduler/gpu` の Filter→Score→Select pipeline とする。この RFC ではコード移植を行わない。将来の実装では GPU だけでなく CPU、memory、network、locality、consent、runner trust を同じ pipeline に載せる。

`ato-coordinator` の `SelectEngine` は first-fit online であり、Ato の PlacementGraph の正本にはしない。利用する場合も `PlacementReason::FallbackFirstFit` として audit/receipt に残す。

## Relationship to Runner API

Runner API は namespace node を実機状態に materialize するための control surface である。`ato-coordinator` の `Provider` / `Engine` / `EngineCommand` は原型として参照するが、次の危険は持ち込まない。

- placeholder token
- untyped payload
- in-memory-only command queue
- SQLite-only storage coupling

必須 operation:

| Runner API | Namespace との関係 |
|---|---|
| `register_runner` | `/runners/<runner_id>` に `RunnerNode` と `RunnerEnrollment` を作る。 |
| `heartbeat` | `RunnerHealth` を更新し、stale runner を placement から外す。 |
| `report_capabilities` | `RunnerCapabilitySnapshot` を append-only snapshot として保存する。 |
| `prepare` | `RunnerCommandPayload::PrepareSession` として配送し、artifact / storage / network / secret projection と launch envelope readiness を確認する。 |
| `build_artifact` | source/ref から `ArtifactBuildId` と `/artifacts/...` を materialize する。 |
| `create_instance` | `/instances/<install_profile_key>` と runner-side state/volume を接続する。 |
| `claim_command` | runner が `RunnerCommandQueue` から認可済み command を claim する。 |
| `complete_command` | command result を session / receipt / audit node に反映する。 |
| `start_session` | `PrepareSession` 成功後に `RunnerCommandPayload::StartSession` として typed payload で配送する。 |
| `stop_session` | session detach、route cleanup、receipt collection に接続する。 |
| `route_port` | `SessionRoute` と `PortBinding` を更新する。 |
| `collect_logs` | `/sessions/<session_id>/logs` に log cursor と hash を append する。 |
| `collect_receipt` | `/sessions/<session_id>/receipt` と `/receipts/...` を接続する。 |
| `snapshot_state` | instance-scoped `StateBinding` の backend snapshot を作る。 |

`RunnerCommandPayload` は tagged enum / protobuf oneof / TypeScript discriminated union のいずれかで表現する。`Payload interface{}` や `Record<string, unknown>` を domain boundary に置いてはならない。

`RunnerCommand` の lifecycle は `Queued -> Claimed -> Running -> Succeeded|Failed|Expired|Cancelled` を基本形にする。`Claimed` は runner が command を lease したが、まだ実行開始 ack を返していない状態である。`Running` は runner が実行開始 ack を返した状態である。`claim_command` は lease を発行し、`lease_until` を過ぎた command は retry 可能に戻すか `Expired` にする。runner は `idempotency_key` を使って重複 `StartSession` / `StopSession` / `RoutePort` を安全に扱う。`complete_command` は `RunnerCommandResult`、attempt、result receipt/log refs を atomic に更新する。

enrollment token は raw token を一度だけ発行し、control plane には hash と TTL と claimed state だけを保存する。`gumball-cloud` の `enrollment_tokens` DDL は design reference とするが、hash algorithm と storage は Ato 側で決める。

`RunnerEnrollment` の lifecycle は `Issued -> Claimed -> Active` または `Issued -> Expired`、`Active -> Revoked` を基本形にする。revoked runner は新しい command を claim できず、既存 session は policy に従って drain または stop される。

## Relationship to ManagedCloud

ManagedCloud は special-case infrastructure ではなく、`RunnerClass::ManagedRunner` の runner として登録する。v0 の推奨実装候補は Cloudflare を control plane、Fly Machines を execution plane、R2 を artifact/log/receipt 補助 store とする。ただし Resource Namespace は Cloudflare / Fly に依存しない。

ManagedCloud v0 の mapping:

| Ato concept | ManagedCloud v0 mapping |
|---|---|
| `RunnerNode` | `/runners/<runner_id>` with `runner_class = managed_runner` |
| `RunnerMetadata.placement` | `Managed { provider: "fly", region: "<region>" }` |
| `RunnerMetadata.transport` | `PublicRoute` または Ato-managed routing |
| `InstanceNode` | install profile と optional Fly volume / state binding の対応 |
| `SessionNode` | one running capsule session, typically one Fly Machine |
| `ArtifactNode` | R2 / artifact store 上の verified build output |
| `ReceiptNode` / `LogNode` | runner から control plane へ収集された execution record |

ManagedCloud v0 は scope を絞る。

- GitHub public repo を優先する。
- Web app / service capsule を優先する。
- Linux x86_64 を初期 target にする。
- 1 capsule = 1 running session から始める。
- persistent state は小容量 volume または object-backed state binding に限定する。
- GPU、private repo、multi-region HA、arbitrary TCP inbound、enterprise VPC peering は v0 に入れない。

Cloudflare Containers、Cloud Run、Modal、AWS、user-owned Fly は、初期の canonical runner backend ではなく追加 runner adapter として扱う。Browser Runner は ManagedCloud runner ではなく、current browser tab / PWA に束縛された ephemeral runner である。

## Runner Classes

v0 の runner class:

| Runner class | 役割 | v0 notes |
|---|---|---|
| `managed_runner` | Ato Cloud 上の default execution endpoint | Sign up 直後に作成できる。Web Console first の default。 |
| `desktop_runner` | ユーザー所有 PC 上の軽量 runner | Desktop UI ではなく headless/minimal agent。pending enrollment から join する。 |
| `external_runner` | BYOC / SSH / Codespaces / E2B / user-owned Fly など | Runner API の追加実装として扱う。 |
| `browser_runner` | 現在の browser tab / PWA を使う ephemeral runner | HTML/CSS/JS/WASM、SPA、軽量 demo 用。server process、Docker、任意 TCP listen、secret 処理は不可。 |
| `browser_preview_runner` | Store preview / public preview 用 runner | `ato-playground` の CSP、TVM、quota、telemetry を design reference にする。 |

Web Console は control surface であり、runner ではない。ただし Web Console で開いている browser session は、capability が満たされる場合に `browser_runner` として登録できる。Web Console device を作る場合は `DeviceNode.can_execute = false` を基本にし、実行能力は `RunnerNode` に分離する。

## Session Lifecycle

`SessionLifecycle` は runner-local process state ではなく、control plane が観測する session node の lifecycle である。v0 の基本遷移は次の通り。

```text
Planned -> Starting -> Running -> Stopping -> Stopped -> Finalized
Planned -> Expired
Starting -> Failed
Running -> Failed
Stopping -> Failed
```

`Stopped` は runner が stop を受理し、receipt/log collection が可能な状態を表す。`Finalized` は reconciler による route cleanup、port release、terminal receipt/log link の記録が完了した UI/API-facing terminal state を表す。terminal state に入った session は再利用しない。同じ launch profile を再実行する場合は新しい `session_id` を発行する。

## Relationship to Store / Install Profiles

Store listing は `/capsules/...` に解決される。GitHub source 由来の capsule は `/capsules/github.com/<owner>/<repo>/<commit>`、Ato Store 由来の capsule は `/capsules/ato.run/<publisher>/<app>/<version>` を初期形とする。

install 後は `/instances/<install_profile_key>` が作られる。revision は `/instances/<install_profile_key>/revisions/<install_revision_id>` に保存し、Launch Profile は `/instances/<install_profile_key>/profiles/<profile_id>` に保存する。

Launch Profile は argv/env の集合ではない。`LaunchProfileNode` は `LaunchRequirementGraph`、`RunnerSelector`、`DeviceSelector`、storage binding requirements、secret requirements、network policy を持つ。これにより同じ app でも preview profile は Browser Runner、default profile は Managed Runner、local-dev profile は Desktop Runner、production profile は Managed Runner only のように切り替えられる。

`/instances/<install_profile_key>/sessions`、`/instances/<install_profile_key>/secrets`、`/instances/<install_profile_key>/state` は instance から見た view である。session、secret ref、log、receipt の正本は top-level canonical node に置き、instance 配下は `ResourceLink` で接続する。

shortcut / dashboard / mobile control surface は `install_profile_key` を安定参照する。ユーザーが日常的に触る単位は capsule artifact ではなく instance である。

state は instance-scoped を原則にする。secrets は secret-ref のみ namespace に置き、実値は secret manager または platform keychain に置く。consent は instance-scoped を原則にし、profile や session に投影する。

`CapsuleArtifact` と Store listing の canonical link は v0 の open question とする。少なくとも artifact content hash、publisher/version、install revision は別 node として追跡できるようにする。

## Relationship to Store and Device Surfaces

Store、Web Console、mobile control surface は Resource Namespace を直接の公開 URL scheme として露出しない。これらの surface は `install_profile_key`、`session_id`、`receipt_id` などの stable id を API 経由で参照し、control plane が内部で ResourcePath に解決する。

Store listing から install される流れでは、公開 catalog identity は `/capsules/...` に解決され、install 後に `/instances/<install_profile_key>` が作られる。Desktop / mobile は instance を primary handle とし、session 一覧、current runner、logs、receipt は `ResourceLink` を辿って表示する。

Sign up 時の default flow は、user profile、default subject/workspace、default runtime network、default managed runner を作る。Desktop Runner は必須ではなく、ユーザーが local execution を選ぶ場合だけ pending enrollment として追加する。

## Relationship to Runtime Network DNS

Runtime Network 内の device name / runner name は公開 DNS ではない。v0 は `/runtime_networks/<runtime_network_id>/dns/<record_id>` を private DNS registry として扱い、source of truth は control plane store に置く。

DNS record は account global ではなく `runtime_network_id` scoped にする。personal / team / org / project network が分かれても名前衝突を避けるためである。

`RuntimeDnsRecordNode` は単純な device name to IP map ではなく、stable name から route endpoint への projection である。

```text
koh-macbook.<runtime-network>.ato.internal
  -> /devices/<device_id>
  -> active runner: /runners/<runner_id>
  -> current endpoint: private IP, relay route, or session route
```

実際の解決は Ato local resolver、ato-netd、MagicDNS、Cloudflare private DNS、relay resolver などへ投影してよい。ただし public session URL は `SessionRoute.public_url` で扱い、private runtime DNS と混ぜない。

## Security Model

`NamespaceAcl` は node ごとの operation-level authorization を表す。v0 は最小 ACL として owner、subject、operation、runner trust requirement を持つ。`magnetic` の `PermissionSet` presets は taxonomy の design reference とし、Ato では instance、runner、session に合わせて抽象化する。

`AuthContext` は namespace operation の caller context を表す。subject、device、runner、auth session、access grants を同時に評価し、device 横断 / runner 横断で session、state、secret に触る権限を明示する。

`AccessGrantNode` は subject が namespace operation を実行する権限を表す。`ConsentGrantNode` は app requirement が concrete resource に binding されることへの user consent を表す。Launch 時は `ConsentGrantNode.grants` と `RequirementBinding` が一致していることを確認する。

すべての user-provided path は path traversal rejection を通す。`..`、`.`、empty segment、OS path、URL path は ResourcePath として拒否する。confused deputy を避けるため、caller の subject、対象 path、要求 operation、runner trust level、instance consent scope を同時に検証する。

Secret value は namespace node に保存しない。`SecretRefNode` は secret backend、opaque ref、version、redaction policy だけを持つ。log、receipt、audit への secret 混入は redaction policy と append 時 validation で拒否する。

Secret projection policy:

- User / Web Console は secret value を作成・更新できる。
- Runner は secret value を直接読まない。launch materialization 時に必要な場合だけ runner-specific projection を受ける。
- Browser Runner への secret injection は v0 では禁止する。
- Secret projection は `RequirementBinding` と `LaunchMaterializationRecord.projection_digests` に残す。

Runner trust level は ManagedCloud、Desktop Runner、External Runner、Browser Runner、BYOC で異なる。PlacementGraph は trust level を constraints として扱い、Runner API は enrollment token と command authorization で trust boundary を固定する。

audit event は append-only であり、receipt linkage と一緒に保存する。`cupbear` の HMAC-chained audit record と、`capsuled` の content-hash chained `AuditEvent` は design reference とする。Ato では `prev_hash`、`event_hash`、必要に応じた HMAC/signature を持つ tamper-evident record として抽象化する。

v0 の audit chain scope は `owner_subject` 単位とする。各 owner subject は独立した append-only chain を持つ。high-value events、たとえば runner enrollment、secret binding、receipt record、placement decision、command completion は resource path ごとにも `latest_event_hash` を metadata に持つ。global chain は v0 では要求しない。

runner enrollment token は raw value を再表示しない。control plane は token hash、TTL、used/claimed state、runner ref を保存する。`gumball-cloud` の `enrollment_tokens` DDL は、TTL と partial uniqueness の考え方を参照する。

command queue は署名または明示的な認可を必要とする。runner は自分の queue だけを claim でき、command payload 内の resource ref が runner trust と consent scope を満たす場合だけ実行できる。

log / receipt の改ざん検知は content hash、cursor monotonicity、receipt signature、audit chain で行う。bounded ring の live log は cache として扱い、canonical log/receipt 保存先とは分ける。

## Persistence Model

v0 は特定の DB 製品を必須にしない。Postgres / D1 / SQLite / local JSON のどれかを RFC 時点で断定せず、抽象境界を固定する。

control plane DB に保存するもの:

- `ResourceNode` の canonical path、kind、metadata、ACL。
- `ResourceLink`、`BackendBinding`、canonical node と view node の関係。
- `DeviceNode`、`RuntimeNetworkNode`、`RuntimeDnsRecordNode`。
- `StorageNode`、`AuthSessionNode`、`AccessGrantNode`、`ConsentGrantNode`、`NetworkPolicyNode`。
- `LaunchRequirementGraph` と `RequirementBinding`。
- `InstallRevision`、`RequirementGraphSnapshot`、`BindingAssignmentSet`、`LaunchTemplate`、`CompatibilityIndex`。
- `RunnerEnrollment`、`RunnerCapabilitySnapshot`、`RunnerHealth`。
- `RunnerCommandQueue` の command、claim、completion。
- `PlacementDecision`。
- instance、revision、launch profile、state binding、secret ref。
- `LaunchMaterializationRecord`。
- `SessionLifecycle` と route cleanup / port release の terminal state。

local desktop state に保存するもの:

- Desktop local-only mode の namespace cache。
- platform keychain への secret ref mapping。
- local runner health cache。
- offline launch profile cache。

artifact store / R2 / S3 に置くもの:

- `.sync` archive、Wasm bundle、OCI metadata、source snapshot。
- build output、dependency output、artifact manifest。
- large `InstallReceipt` payload。
- large receipt payload。
- long-term log object。
- artifact signature と content hash metadata。

runner 側に一時的にしか置かないもの:

- runner-local process id。
- ephemeral port allocation。
- live log buffer。
- command execution scratch。
- transient health metrics。

receipts/logs/audit の保存先:

- receipt と audit は canonical store に append する。
- live logs は runner-local または control plane ring buffer を cache として持ってよい。
- long-term logs は object store または log sink に置き、namespace node は `LogNode` ref を持つ。

cache と canonical state は分ける。`RunnerCapabilitySnapshot` の最新値 cache は placement に使えるが、監査に必要な decision input は `PlacementDecision` に snapshot ref として保存する。

`LaunchTemplate` は canonical install output であり、session cache ではない。`LaunchMaterializationRecord` は session-scoped record であり、次回 launch に再利用してはならない。Desktop/local で `instances/`、`revisions/`、`artifact-cache/` の file layout を使う場合も、Cloud/Web Console で DB + object store に分割する場合も、この三層を混ぜない。

## Minimal v0 Scope

v0 に入れるもの:

- `ResourcePath`
- `ResourceNode`
- `ResourceLink`
- `BackendBinding`
- `NamespaceAcl` minimal
- `RunnerNode`
- `RunnerMetadata`
- `RunnerCommandResult`
- `BrowserRunnerSandbox` minimal
- `LaunchRequirementGraph`
- `RequirementRelation`
- `RequirementBinding`
- `RequirementResolver` minimal interface
- `BindingCandidate`
- `BindingPlan`
- `LaunchResolutionPlan`
- `ConsentGap`
- `ProvisioningAction`
- `InstallRevision`
- `ArtifactBuild`
- `RequirementGraphSnapshot`
- `BindingAssignmentSet`
- `LaunchTemplate`
- `LaunchTemplateKey`
- `CompatibilityIndex`
- `InstallReceipt`
- `StorageNode`
- `StorageCapabilities`
- `AuthContext`
- `AccessGrantNode`
- `ConsentGrantNode`
- `NetworkPolicyNode`
- `IoRequirement`
- `IoCapability`
- `DeviceNode`
- `RuntimeNetworkNode`
- `RuntimeDnsRecordNode`
- `InstanceNode`
- `SessionNode`
- `SessionLifecycle`
- `ArtifactNode`
- `ReceiptNode`
- `SecretRefNode`
- `RunnerEnrollment` lifecycle
- `RunnerCapabilitySnapshot`
- `RunnerCommand` typed payload
- `RunnerCommandPayload::PrepareSession`
- `RunnerCommand` lifecycle, lease, idempotency
- `PlacementDecision` record
- `LaunchMaterializationRecord`
- `SessionRoute`
- `PortBinding`
- `AuditEvent`

v0 に入れないもの:

- FUSE
- 9P
- WebDAV
- full mesh networking
- arbitrary TCP tunnel
- Headscale-compatible server
- billing revenue allocation
- publisher revenue transfer
- automatic state migration
- cross-runner live migration
- GPU scheduler implementation
- Browser Runner full filesystem access
- Browser Runner server process / Docker support
- OS device driver implementation
- kernel-level syscall mediation
- USB / serial / HID direct driver
- Cloudflare / Fly / Modal / Cloud Run adapter implementation

## Asset Reuse Plan

| Asset | Relevant files | Use as | Ato concept | Adopt now? | Risk |
|---|---|---|---|---|---|
| `ato-vfs` VirtualFileSystem / MountTable / VirtualPath / SecurityManager | `capsuled-archives/core/ato_wasm/vfs/src/vfs/{mod.rs,mount.rs,path.rs,security.rs}` | Design reference | `ResourcePath`, `ResourceNode`, `MountTable`, `NamespaceAcl` | Yes, type extraction first | WASM runtime 内 VFS なので control plane namespace に reshape が必要。`MountTable::add_mount` の完成度は要精査。 |
| `sync-rs` VfsMount / VfsEntry / SyncSignature | `capsuled-archives/sync-rs/crates/sync-fs/src/vfs.rs`, `capsuled-archives/sync-rs/crates/sync-format/src/verification.rs` | Design reference | `StateBinding`, `CapsuleArtifact`, `ArtifactBuildId`, `ReceiptRef` | Yes, shape only | FUSE/WebDAV adapter は v0 では不要。`ManifestPermissions` は path-level ACL ではない。 |
| `ato-coordinator` Provider / Engine / EngineCommand | `capsuled-archives/ato-coordinator/control-plane/internal/provider/provider.go`, `internal/service/engine_service.go` | Design reference | `RunnerAdapter`, `RunnerEnrollment`, `RunnerCapabilitySnapshot`, `RunnerCommandQueue` | Yes, typed rewrite | placeholder token、untyped payload、SQLite coupling、in-memory queue を除去する必要。 |
| `capsuled` GPU Scheduler Filter→Score→Select | `onescluster/capsuled/client/pkg/scheduler/gpu/{scheduler.go,filters.go,scorers.go,types.go}` | Design reference | `PlacementGraph`, `PlacementCandidate`, `PlacementDecision` | Defer implementation | Go 実装を直接移植しない。v0 は decision record と first-fit fallback まで。 |
| `capsuled` Reconciler | `onescluster/capsuled/client/pkg/reconcile/{capsule_reconciler.go,reconciler.go}` | Design reference | `SessionReconciler`, route cleanup, port release | Defer implementation | namespace node 一般に抽象化する必要。 |
| `gumball-cloud` enrollment_tokens | `onescluster/gumball-cloud/backend/migrations/20251223120000_add_enrollment_tokens.sql` | Design reference | `RunnerEnrollment` | Yes, schema reference | sha256 hash は強化検討。DB 製品依存は避ける。 |
| `magnetic` PermissionSet | `magnetic/tauri-p2p/crates/magnetic-core/src/environment/permissions.rs` | Design reference | `NamespaceAcl` presets | Yes, taxonomy only | Ato の operation-level ACL に再構成が必要。 |
| `cupbear` AuditRecordStored | `cupbear/app/lib/agent/audit.ts` | Design reference | `AuditEvent`, receipt linkage | Yes, shape only | 元実装は in-memory。Ato では永続化と chain verification が必須。 |
| `ato-playground` edge worker / Theater UI | `ato-play-edge`, `ato-play-web` 系 | Design reference | `BrowserPreviewRunner`, sandbox CSP, quota telemetry | Defer implementation | Browser Runner は万能 runner ではない。Store preview 用 policy と user session runner を混同しない。 |

`Direct port` はこの RFC では選ばない。最初の作業は移植ではなくコード精査と型抽出である。

## Anti-patterns / Do Not Reuse

- XOR encryption stub: 暗号境界を満たさず、通信 security を実装済みのように見せるため Ato に入れてはいけない。
- silent degraded success: 障害を成功として返すと placement、reconciliation、audit が実状態を誤認する。
- tailscale/headscale CLI shell-out without typed errors: 外部 CLI の stdout/stderr 依存は typed control plane error と retry policy を壊す。
- placeholder enrollment token: runner enrollment の trust boundary を無効化し、登録監査もできない。
- untyped command payload: RunnerCommand の互換性、認可、監査、migration を壊す。
- hardcoded plan/pricing constants: billing と quota policy をコード release に固定し、運用変更と監査を難しくする。
- giant match/switch job processor: command ごとの責務と retry/error semantics が混ざり、runner domain が god object 化する。
- god object capsule manager: session、artifact、network、state、logs の責務が一体化し、namespace 境界を壊す。
- in-memory-only session store: control plane restart、desktop reconnect、mobile observation で session の正本を失う。
- egress proxy placeholder parser: CONNECT や TCP stream の不完全 parse は security boundary と diagnostic を壊す。
- disabled nacelle daemon interface: 旧 daemon 構成を復活させると現行 Ato の runtime routing と責務分離を乱す。

## Migration / Implementation Plan

PR 1: `ResourcePath` + `ResourceNode` core types

- やること: canonical path parser、global uniqueness invariant、node kind/payload split、resource ref、`ResourceLink`、`BackendBinding`、最小 error 型を追加する。
- やらないこと: FUSE / 9P / WebDAV、OS path mapping、Runner API 実装。

PR 2: Namespace store interface + in-memory test backend

- やること: `get`、`list`、`resolve`、`mount`、`unmount` の interface と test backend を作る。
- やらないこと: DB 製品の固定、production migration、artifact store 実装。

PR 3: `RunnerNode` + `RunnerCapabilitySnapshot`

- やること: `/runners/<runner_id>`、`RunnerMetadata`、capability snapshot、health、trust level を namespace node に接続する。
- やらないこと: GPU scheduler 実装、runner 起動。

PR 4: typed `RunnerCommandQueue`

- やること: `RunnerCommandPayload` と `RunnerCommandResult` を tagged enum / oneof として定義し、status、lease、retry、idempotency、claim/complete の domain model を固定する。
- やらないこと: placeholder token、untyped JSON payload、in-memory-only queue を正本にしない。

PR 5: `InstanceNode` + `StateBinding` + `SecretRef`

- やること: install profile、revision、launch profile、state binding、secret ref を instance-scoped に接続する。
- やらないこと: secret value 保存、自動 state migration、cross-runner migration。

PR 6: `SessionNode` + `SessionRoute` + `PortBinding`

- やること: session path、`SessionLifecycle`、runner projection、ports/logs/receipt/execution_identity refs、instance backrefs を追加する。
- やらないこと: arbitrary TCP tunnel、full mesh networking、Headscale-compatible server。

PR 7: `ReceiptRef` + `AuditEvent` append

- やること: receipt ref、owner_subject scoped audit event hash chain、resource path `latest_event_hash`、log cursor metadata を append-only model にする。
- やらないこと: billing revenue allocation、publisher revenue transfer。

PR 8: `PlacementDecision` record integration

- やること: placement constraints、candidate refs、selected runner/session、reason を記録する。
- やらないこと: Filter→Score→Select scheduler の本実装。first-fit は fallback として明示する。

PR 9: `LaunchMaterializationRecord`

- やること: profile、input refs、projection digests、execution id を `/sessions/<session_id>/execution_identity` に接続する。
- やらないこと: Execution Identity の再定義、diagnostic facts の execution drift 扱い。

PR 10: `DeviceNode` + runtime network DNS registry

- やること: `/devices/<device_id>`、`/runtime_networks/<runtime_network_id>/dns/<record_id>`、private name to route endpoint projection を追加する。
- やらないこと: Runtime Network 全体、Headscale-compatible server、public DNS routing。

PR 11: Managed / Browser runner metadata

- やること: `managed_runner`、`desktop_runner`、`external_runner`、`browser_runner`、`browser_preview_runner` の metadata schema と v0 constraints を固定する。
- やらないこと: Cloudflare/Fly 固有 API 実装、Browser Runner の full filesystem / server process / arbitrary TCP support。

PR 12: Application requirement graph

- やること: `LaunchRequirementGraph`、`RequirementNode`、`RequirementBinding` を追加し、app requirement と namespace resource を分離する。
- やらないこと: capsule manifest parser の全面変更、runner implementation。

PR 13: Storage / auth / consent nodes

- やること: `/storage/<storage_id>`、`/auth/sessions`、`/auth/grants`、`/consents`、`/network_policies` を追加する。
- やらないこと: S3/R2/Fly volume adapter 実装、OAuth provider 実装、secret value storage。

PR 14: I/O capability requirements

- やること: `IoRequirement`、`IoCapability`、I/O consent、receipt observation の最小 model を追加する。
- やらないこと: OS driver、kernel mediation、USB/HID/serial direct driver 実装。

PR 15: Requirement resolution algorithm

- やること: `RequirementResolver`、`BindingCandidate`、`BindingPlan`、`LaunchResolutionPlan`、`ConsentGap`、`ProvisioningAction` を追加し、Compile -> Resolve -> Place -> Materialize の staged resolver を固定する。
- やらないこと: SAT solver / ILP、GPU scheduler 実装、候補探索中の namespace mutation。

PR 16: consent checkpoint + runner prepare/start

- やること: `ConsentRequired(plan_id, gaps)`、plan revalidation、materialization transaction、`RunnerCommandPayload::PrepareSession` と `StartSession` の分離を追加する。
- やらないこと: OAuth provider 実装、secret value injection の一般解禁、partial materialization の許容。

PR 17: install outputs + launch reuse cache

- やること: `InstallRevision`、`RequirementGraphSnapshot`、`BindingAssignmentSet`、`LaunchTemplate`、`LaunchTemplateKey`、`CompatibilityIndex`、`InstallReceipt` を追加し、install-time fixed output と launch-time revalidation の境界を固定する。
- やらないこと: launch ごとの source rebuild、session 固有 field の template cache 混入、secret value の cache key 化。

## Example Flows

Sign up:

1. User profile と default subject/workspace を作る。
2. `/runtime_networks/<runtime_network_id>` を作る。
3. `/devices/<device_id>` を Web Console device として作り、`can_execute = false` にする。
4. `/runners/<runner_id>` を `managed_runner` として作り、Web Console から利用できる default runner にする。
5. Desktop Runner は必要な場合だけ pending enrollment として追加する。

Browser runner:

1. Web Console が browser capability を probe する。
2. HTML/CSS/JS/WASM などの軽量 capsule に限定して `browser_runner` を候補にする。
3. `BrowserRunnerSandbox` と CSP / quota / telemetry policy を runner metadata に記録する。
4. server process、Docker、任意 TCP listen、secret value を必要とする capsule は placement から除外する。

Install:

1. Store listing を `/capsules/ato.run/<publisher>/<app>/<version>` に解決する。
2. artifact を verify し、`/artifacts/blake3/<hex>` に `ArtifactNode` を作る。
3. `/instances/<install_profile_key>`、`/revisions/<install_revision_id>`、`/profiles/<profile_id>` を作る。
4. state contract と secret ref を instance-scoped consent に紐づける。

Launch:

1. launch profile から effective `LaunchRequirementGraph` を作る。
2. requirement resolver が non-runner resources、derived runner constraints、consent gaps、provisioning actions を含む `LaunchResolutionPlan` を作る。
3. consent gaps があれば `ConsentRequired(plan_id, gaps)` を返し、承認後に plan を revalidate する。
4. `BindingPlan` と `/runners/<runner_id>` の `RunnerMetadata` / `RunnerCapabilitySnapshot` を候補にして `PlacementDecision` を記録する。
5. materialization transaction で `/sessions/<session_id>`、instance backref、network policy、consent、`LaunchMaterializationRecord`、audit event を作る。
6. typed `RunnerCommandPayload::PrepareSession` を queue に入れ、projection digest / readiness を保存する。
7. typed `RunnerCommandPayload::StartSession` を queue に入れる。

Receipt and stop:

1. runner が logs と receipt を収集し、`/logs/<log_id>` と `/receipts/<receipt_id>` に正本を作る。
2. `/sessions/<session_id>/logs/<log_id>` と `/sessions/<session_id>/receipt` に `AttachedReference` を作る。
3. stop command completion 後、`SessionLifecycle` を terminal state に進める。
4. route cleanup と port release の結果を audit chain に append する。

Concrete app examples:

1. HTML + WASM lightweight app

   Requirements:

   ```toml
   [requires.runtime]
   kind = "browser"
   wasm = true
   web_worker = true

   [requires.network]
   mode = "proxy_only"
   allow = ["https://api.example.com"]
   ```

   Materialization:

   ```text
   runtime requirement -> /runners/run_browser_...
   network requirement -> /network_policies/netpol_proxy_...
   session -> /sessions/ses_.../runner -> /runners/run_browser_...
   ```

   Resolution:

   ```text
   RuntimeResolver -> browser_runner, wasm=true, web_worker=true constraints
   NetworkResolver -> CreateNetworkPolicy(proxy_only), enforcement=AtoProxyOnly
   Runner filtering -> current browser runner passes if capability probe matches
   Score -> browser runner wins over managed runner for browser profile
   Commit -> session, network policy, LaunchMaterializationRecord
   ```

   Effect: Web Console 内で実行できる。外部通信は Ato proxy 経由のみ。browser session 終了時に session も終了する。

2. `pgweb` style DB viewer

   Requirements:

   ```toml
   [requires.runtime]
   kind = "server_process"
   runtime = "oci"

   [requires.secret.database_url]
   kind = "env"
   required = true

   [requires.network]
   mode = "proxy_or_runner_policy"
   allow = ["postgres://db.example.com:5432"]

   [profiles.default.runner]
   allow_classes = ["managed_runner", "desktop_runner"]
   deny_classes = ["browser_runner"]
   ```

   Materialization:

   ```text
   runtime requirement -> /runners/run_managed_...
   secret.database_url -> /secrets/sec_database_url_...
   network requirement -> /network_policies/netpol_pg_...
   ```

   Resolution:

   ```text
   RuntimeResolver -> browser_runner rejected because server_process is unsupported
   SecretResolver -> /secrets/sec_database_url, browser_runner rejected because v0 secret injection is forbidden
   NetworkResolver -> CreateNetworkPolicy(postgres egress under runner policy)
   Runner filtering -> managed_runner and active desktop_runner can pass
   Consent checkpoint -> database_url projection to selected runner class must be approved
   Commit -> secret projection digest is included in execution identity inputs
   ```

   Effect: Browser Runner は除外される。secret は SecretRef として保存され、runner には materialization 時に必要最小限の projection だけ渡す。

3. S3-compatible image processing app

   Requirements:

   ```toml
   [requires.storage.input]
   kind = "object_store"
   access = ["read", "list"]

   [requires.storage.output]
   kind = "object_store"
   access = ["write"]

   [requires.compute]
   gpu = "optional"
   ```

   Materialization:

   ```text
   storage.input -> /storage/stor_s3_... prefix=input/
   storage.output -> /storage/stor_s3_... prefix=output/
   storage credential -> /secrets/sec_s3_...
   session -> /sessions/ses_image_...
   ```

   Resolution:

   ```text
   StorageResolver -> existing /storage/stor_s3_... or CreateStorageBinding
   SecretResolver -> credential ref required for storage access
   Runner constraints -> runner must reach storage endpoint and satisfy optional GPU preference
   Score -> managed GPU runner may win, but GPU absence is not hard reject
   Receipt -> storage refs, prefixes, and projection digest are recorded
   ```

   Effect: storage は state と別の external resource として扱える。profile ごとに local storage / S3 / R2 を切り替えられる。receipt には storage ref と prefix が残る。

4. Camera browser app

   Requirements:

   ```toml
   [requires.runtime]
   kind = "browser"

   [requires.input.camera]
   class = "camera"
   access = ["read"]
   mediated_by = "browser_api"
   ```

   Materialization:

   ```text
   runtime requirement -> /runners/run_browser_...
   camera input -> browser permission mediated IoRequirement
   session -> /sessions/ses_camera_...
   ```

   Resolution:

   ```text
   IoResolver -> no standalone camera resource; requirement is satisfied by runner capability + browser consent
   Consent checkpoint -> camera permission may be requested before or during browser start
   Binding -> resolved_resources includes runner/device refs, enforcement is BrowserApi permission
   Receipt -> requested / allowed / denied observation is recorded
   ```

   Effect: Ato は camera driver を持たない。browser permission API を通じて許可される。receipt には camera capability requested / allowed / denied を残す。

## Open Questions

- Resource Namespace の reference implementation を Rust / TypeScript / DB schema のどこに置くか。
- Desktop Runner local-only mode と cloud control plane mode で同じ `ResourcePath` を使うか。
- D1 / Postgres / SQLite のどれを v0 control plane store にするか。
- `RunnerCapabilitySnapshot` の redaction policy。
- `SecretRef` の canonical form。
- Receipt と `AuditEvent` の署名方式。
- `PlacementDecision` の保存場所。
- `StateBinding` と Execution Identity の関係。
- Store listing と `CapsuleArtifact` の canonical link。
- Mobile control surface が直接読む namespace API の範囲。
- consent を instance-scoped に固定した場合の profile override の扱い。
- 将来 `/subjects/<subject_id>/...` view を追加するか、top-level canonical path + `owner_subject` invariant を維持するか。
- `RunnerMetadata` を canonical JSON として digest 化するか。
- Browser Runner の capability probing と redaction policy。
- Runtime DNS をどの resolver backend に最初に投影するか。
- `LaunchTemplate` と `BindingAssignmentSet` を DB row と object payload のどちらに置くか。
- compatibility index refresh をどの event で起動するか。
- `capsule_instance_key` の canonical encoding と保存場所。

## Validation

RFC 作成後、次を実行して主要語彙が文書内に存在することを確認する。

```bash
rg -n "Resource Namespace|ResourcePath|LaunchRequirementGraph|RequirementBinding|LaunchResolutionPlan|BindingPlan|ConsentGap|ProvisioningAction|InstallRevision|ArtifactBuild|BindingAssignmentSet|LaunchTemplate|CompatibilityIndex|PrepareSession|StorageNode|AuthContext|IoRequirement|RunnerMetadata|RunnerCapabilitySnapshot|RunnerCommand|PlacementDecision" docs/rfcs/draft/ato-resource-namespace.md
```
