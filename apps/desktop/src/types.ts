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
}
// The only settings the dialog may write; version and package hint stay host-owned.
export interface EditableSettings {gui_log_limit:number; ocr_environment:OcrEnvironment|null; notifications:NotificationPreferences; locale:Locale}
// Host-issued session identity; revisions increment on reinspect and ids are never reused.
export interface WorkspaceRef {workspace_id:string; revision:number}
export interface Selection extends WorkspaceRef {package:PackageInfo; profiles:Profile[]; profiles_error:Fault|null; package_path:string}
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
