import type {Locale} from './i18n.ts';

export type Json = null | boolean | number | string | Json[] | {[key: string]: Json};
export interface Fault {category:string; message:string; context:Json}
export interface Schema {
  type:'object'|'array'|'string'|'number'|'integer'|'boolean';
  properties?:Record<string,Schema>; required?:string[]; items?:Schema;
  enum?:Json[]; default?:Json; minimum?:number; maximum?:number;
  minItems?:number; maxItems?:number; minLength?:number; maxLength?:number;
}
export interface PackageInfo {
  package_id:string; inventory_identity:string; schema_identity:string;
  runtime:string; schema:Schema; profiles:Record<string,{options:Record<string,Json>}>;
  effective_defaults:Record<string,Json>|null;
  target:TargetDeclaration|null; target_identity:string|null;
}
export interface Profile {version:number; id:string; name:string; package_id:string; schema_identity:string; values:Record<string,Json>}
// Machine-local OCR environment; absent means unconfigured. Field order matches the backend struct.
export interface OcrEnvironment {
  model:string; profile:string; language:string; provider:string; runtime_profile:string;
  model_root:string; runtime_path:string; native_library_paths:string[];
}
// Finite choices validated in Rust: visible_count 1|2, timeout_seconds 5|8|12.
export interface NotificationPreferences {visible_count:number; timeout_seconds:number; show_success:boolean}
export interface Settings {
  version:number; gui_log_limit:number; package_path:string|null; ocr_environment:OcrEnvironment|null;
  notifications:NotificationPreferences; locale:Locale;
  // Absent or null means the default `<root>/backups` destination.
  backup_directory:string|null;
}
// The only settings the dialog may write; version and package hint stay host-owned.
export interface EditableSettings {gui_log_limit:number; ocr_environment:OcrEnvironment|null; notifications:NotificationPreferences; locale:Locale; backup_directory:string|null}
// Host-issued session identity; revisions increment on reinspect and ids are never reused.
export interface WorkspaceRef {workspace_id:string; revision:number}
export interface ProfileCatalog {profiles:Profile[]; profiles_error:Fault|null}
// A real inspected package bound to one named Tab session.
export interface Selection extends WorkspaceRef, ProfileCatalog {internal_name:string; display_name:string; package:PackageInfo; package_path:string}
// Persisted package source forms. Only `directory` is inspectable by this desktop; `custom_archive` is retained unsupported.
export type PackageSource = {kind:'directory'; path:string} | {kind:'custom_archive'; path:string};
export interface PackageReference {package_id:string; source:PackageSource}
// The durable Tab record; names and references persist, sessions do not.
export interface TabRecord {version:number; internal_name:string; display_name:string; open:boolean; packages:PackageReference[]; selected_package_id:string|null}
// Host-issued recovery context: an opaque bounded token tied to one Workspace revision. It is never an authoritative
// path/schema/owner tuple and grants recovery actions only, not run or ordinary profile authority.
export interface RecoveryRef {workspace:WorkspaceRef; token:string}
// A safely decoded owned profile the inspected schema rejects, with the current attributed validation issue.
export interface RecoverableProfile {profile:Profile; issue:Fault}
// The Tab's current repair context. `relocation` marks a same-package candidate directory other than the saved source;
// its binding runs the strict compatibility check. `binding_required` is true after a recovery-required or failed
// binding until an explicit retry succeeds: only repair/reset/retry/discard apply, nothing runs from the candidate.
// Both axes are independent: an in-place failed binding is not a relocation, and a bound relocation needs no retry.
export interface RecoveryView {context:RecoveryRef; relocation:boolean; binding_required:boolean; package_path:string; package:PackageInfo; profiles:RecoverableProfile[]; profiles_error:Fault|null}
export type RecoveryStatus = 'saved'|'repair_required'|'storage_failed';
// One profile's publication fact from explicit inspection; a saved outcome stays true if binding fails afterwards.
export interface ProfileRecoveryOutcome {profile_id:string; name:string; status:RecoveryStatus; issue:Fault|null}
// One open Tab as the host sees it: a fresh session identity, a real selection when inspection succeeded, and the
// owner-scoped fault of any saved source independently of a later candidate binding failure. `saved_package` is the
// selected durable reference: kept on failure, updated by a successful bind, or null on a new Tab. It is metadata
// and grants no inspected or run authority. `recovery` is the host's repair context for rejected profiles, present
// with or without a retained selection.
export interface WorkspaceView {workspace_id:string; revision:number; internal_name:string; display_name:string; selection:Selection|null; source_error:Fault|null; saved_package:PackageReference|null; recovery:RecoveryView|null}
// Typed result of explicit inspection or binding retry. `bound` published a Selection; `recovery_required` established
// a non-runnable candidate; `binding_failed` kept the prior selected authority (if any). Outcomes remain per-profile facts.
export interface InspectionOutcome {kind:'bound'|'recovery_required'|'binding_failed'; workspace:WorkspaceView; outcomes:ProfileRecoveryOutcome[]; binding_error:Fault|null}
// Result of one repair Save or confirmed Reset. `saved` is the committed record regardless of the later refresh;
// `draft` is the host's default-based draft when Reset could not form a valid profile; `issue` is the current failure.
export interface RecoveryMutation {saved:Profile|null; draft:Record<string,Json>|null; issue:Fault|null; recovery:RecoveryView|null; catalog:ProfileCatalog|null; refresh_error:Fault|null}
export interface WorkspaceCatalog {open:WorkspaceView[]; closed:TabRecord[]; faults:Fault[]}
// `loading` is the transient first read before the shell has resolved the root; it carries no defaults or catalog.
export type BootstrapState = 'loading'|'setup'|'ready'|'recovery';
// Outcome of one explicit legacy-profile import; a partial batch keeps its committed subset.
export interface LegacyImport {imported:string[]; unchanged:string[]; fault:Fault|null}
// Shell-owned bootstrap truth, independent of any Application. Paths are disclosed deliberately by the host.
export interface BootstrapStatus {
  state:BootstrapState; stage:string; root:string|null; legacy_root:string|null; fault:Fault|null;
  settings:Settings|null; application_available:boolean; pending_restore:boolean; catalog:WorkspaceCatalog|null;
}
export interface SnapshotReceipt {path:string; generation:string; files:number; bytes:number}
export interface ControllerView {
  run:string|null; state:string; operation:string; result:Record<string,Json>|null; error:Fault|null;
  progress:Record<string,Json>[]; dropped_logs:number;
  workspace_id:string|null; workspace_revision:number|null;
}
export interface LogEntry {
  sequence:number; time_ms:number; source:string; level:string; run:string|null; workspace_id:string|null;
  code:string; message:string; fields:Json;
}
export interface LogBatch {entries:LogEntry[]; gui_dropped:number; file_dropped:number; file_errors:number; last_file_error:string|null}
export interface WorkspaceResult {workspace:WorkspaceRef; controller:ControllerView}
// One retained terminal Check with exactly the inputs the host read for it.
export interface RetainedCheck {
  workspace:WorkspaceRef|null; environment:OcrEnvironment|null; descriptor_path:string|null;
  package_inventory_identity:string|null; controller:ControllerView;
}
export interface Poll {controller:ControllerView; logs:LogBatch; workspace_results:WorkspaceResult[]; last_check:RetainedCheck|null}
export interface StartRequest {
  package_path:string; inventory_identity:string; package_id:string; schema_identity:string; profile_id:string;
  values:Record<string,Json>; lane:string; scenario:string; replay_descriptor_path:string|null;
}

export interface TargetDeclaration {id:string; window_title:string|null}
export interface TargetLocation {kind:'executable'|'bundle'; path:string}
export interface TargetInputPolicy {
  route:'process_directed'|'system'; focus:'preserve'|'require_focused';
  pointer_mode:'core_graphics'|'appkit_background'|null; click_hold_ms:number;
}
export interface TargetConfiguration {
  platform:'macos'; game:TargetLocation; launcher:TargetLocation|null; arguments:string[];
  working_directory:string|null; window_title:string; input:TargetInputPolicy;
}
export interface ResolvedLocation {path:string; executable:string}
export interface TargetResolution {game:ResolvedLocation; launcher:ResolvedLocation|null; working_directory:string|null}
export interface TargetBinding {
  id:string; package_id:string; target_id:string; declaration_identity:string;
  configuration:TargetConfiguration; resolution:TargetResolution;
}
export interface TargetRecord {version:1; internal_name:string; package_id:string; revision:number; binding:TargetBinding|null}
export interface TargetExpectation {revision:number; binding_id:string|null}
export interface TargetCheck {
  configuration_identity:string; resolution:TargetResolution; previous_resolution:TargetResolution|null; resolution_changed:boolean;
}
export interface TargetContext {workspace:WorkspaceRef; internal_name:string; package_id:string; declaration_identity:string|null}
export interface TargetView {context:TargetContext; record:TargetRecord; compatible:boolean}
export interface TargetCheckResponse {context:TargetContext; revision:number; binding_id:string|null; check:TargetCheck}
export interface TargetSaveResponse {view:TargetView; check:TargetCheck}
