// Record-scoped service methods surfaced as first-class form buttons (distinct from state actions,
// which use /action/). Each maps a host model to a POST endpoint + how to confirm + how to phrase the
// result. The backend does not yet advertise these, so this small client registry is the seam.

export interface ServiceAction {
  /** POST /api/<model>/<id>/<endpoint>. Relocated cross-record methods live under `service/<name>` (the
   *  generic register_service! route); methods still pinned in the core keep their bare name for now. */
  endpoint: string
  label: string
  /** Confirmation prompt for irreversible operations (none = run immediately). */
  confirm?: string
  /** Builds the success toast from the endpoint's JSON result. */
  resultToast: (result: Record<string, unknown>) => string
}

export const SERVICE_ACTIONS: Record<string, ServiceAction[]> = {
  'sale.order': [
    {
      endpoint: 'service/create_invoice',
      label: 'Create invoice',
      confirm: 'Create a posted invoice for this order?',
      resultToast: (r) => `Invoice created (entry #${r.invoice})`,
    },
    {
      endpoint: 'service/apply_pricelist',
      label: 'Apply pricelist',
      resultToast: (r) => `Repriced ${r.priced} line${r.priced === 1 ? '' : 's'}`,
    },
    {
      endpoint: 'service/apply_taxes',
      label: 'Apply taxes',
      resultToast: (r) => `Taxed ${r.taxed} line${r.taxed === 1 ? '' : 's'}`,
    },
    {
      endpoint: 'service/create_delivery',
      label: 'Create delivery',
      resultToast: (r) => `Delivery created (draft transfer #${r.picking})`,
    },
  ],
  'purchase.order': [
    {
      endpoint: 'service/create_receipt',
      label: 'Create receipt',
      resultToast: (r) => `Receipt created (draft transfer #${r.picking})`,
    },
  ],
  'stock.picking': [
    {
      endpoint: 'validate',
      label: 'Validate',
      confirm: 'Validate this transfer? It moves the stock and cannot be undone.',
      resultToast: (r) => `Transfer ${r.validated} done`,
    },
  ],
  'account.move': [
    {
      endpoint: 'service/post',
      label: 'Post entry',
      confirm: 'Post this journal entry? Posted entries cannot be edited.',
      resultToast: (r) => `Entry ${r.posted} posted`,
    },
  ],
  'product.template': [
    {
      endpoint: 'service/generate_variants',
      label: 'Generate variants',
      resultToast: (r) => `Variants: ${r.created} created, ${r.kept} kept, ${r.archived} archived`,
    },
  ],
}
