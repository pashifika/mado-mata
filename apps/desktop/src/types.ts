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
export interface Settings {version:number; gui_log_limit:number; package_path:string|null}
export interface Selection {package:PackageInfo; profiles:Profile[]; profiles_error:Fault|null}
export interface ControllerView {
  run:string|null; state:string; result:Record<string,Json>|null; error:Fault|null;
  progress:Record<string,Json>[]; dropped_logs:number;
}
export interface LogEntry {
  sequence:number; time_ms:number; source:string; level:string; run:string|null;
  code:string; message:string; fields:Json;
}
export interface LogBatch {entries:LogEntry[]; gui_dropped:number; file_dropped:number; file_errors:number; last_file_error:string|null}
export interface Poll {controller:ControllerView; logs:LogBatch}
