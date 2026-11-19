export function newer(before: MadoObservation, after: MadoObservation): boolean {
  return after.run === before.run && after.attempt === before.attempt &&
    after.process_lifetime === before.process_lifetime && after.session === before.session &&
    after.epoch === before.epoch && after.geometry === before.geometry && after.width === before.width &&
    after.height === before.height && after.frame > before.frame;
}

export function submittedClick(receipt: MadoReceipt): boolean {
  return receipt.status === "Submitted" && receipt.submitted === 1 && receipt.total === 1 &&
    !receipt.partial_native_effect && !receipt.used_fallback && !receipt.cleanup_required.length &&
    !receipt.cleanup?.may_leave_state_held;
}
