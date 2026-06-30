// Wizards available from a host record's form. The backend does not yet advertise wizards in the
// contract, so this small client registry (keyed by host model) is the seam — when it does, this
// becomes the fallback. Each spec names the transient model, its apply endpoint, and the fields to show.

export interface WizardSpec {
  /** The transient model opened via POST /api/<wizardModel>/open. */
  wizardModel: string
  /** POST /api/<wizardModel>/<id>/<applyPath> runs the wizard. */
  applyPath: string
  label: string
  /** Fields to show in the wizard form (others — the seeded context, timestamps — stay hidden). */
  fields: string[]
  resultToast: (result: Record<string, unknown>) => string
}

export const WIZARDS: Record<string, WizardSpec[]> = {
  'sale.order': [
    {
      wizardModel: 'sale.order.discount',
      applyPath: 'service/apply_discount',
      label: 'Discount',
      fields: ['discount'],
      resultToast: (r) => `Discount applied to ${r.applied} line${r.applied === 1 ? '' : 's'}`,
    },
  ],
}
