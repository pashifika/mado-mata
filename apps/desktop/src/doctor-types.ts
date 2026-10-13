// Observation only: routes must re-enter current owner/consent admission.
export interface DoctorPackage {
  package_id: string;
  artifact_id: string;
}

export interface DoctorRequest {
  package: DoctorPackage | null;
}

export type DoctorScope =
  | "root" | "settings" | "transactions" | "tabs" | "tab"
  | "source" | "profiles" | "profile" | "target" | "package_verification";

export type DoctorCode =
  | "checked" | "absent" | "root_unavailable" | "parent_unavailable"
  | "read_refused" | "invalid_document" | "pending_transaction"
  | "recovery_unavailable" | "archive_missing" | "archive_present"
  | "archive_invalid" | "verification_not_requested" | "verified"
  | "source_unexamined" | "unsupported_source" | "legacy_layout_refused"
  | "schema_unexamined" | "profile_incompatible" | "target_unexamined"
  | "target_incompatible" | "bound_exceeded" | "owner_unavailable"
  | "changed_during_read" | "observation_only";

export type DoctorRoute =
  | "retry" | "import_original" | "inspect" | "profile_recovery"
  | "snapshot" | "restore" | "recover_transaction" | "manual";

export interface DoctorOwner {
  internal_name: string | null;
  package_id: string | null;
  artifact_id: string | null;
  profile_id: string | null;
}

export interface DoctorCheck {
  scope: DoctorScope;
  owner: DoctorOwner;
  status: "passed" | "failed" | "unexamined";
  code: DoctorCode;
  fault_category: string | null;
  routes: DoctorRoute[];
  details: { path: string | null };
}

export interface DoctorReport {
  root: string | null;
  checks: DoctorCheck[];
  truncated: boolean;
}
