// Financial reports, all derived from one trial-balance fetch (per-account debit/credit/balance over
// posted entries): the trial balance itself, a profit & loss, and a balance sheet. Read-only.

import { useEffect, useState } from 'react'
import { AlertTriangle } from 'lucide-react'
import * as api from '../api'
import { Card, ErrorState, PageHeader, SkeletonText, Tabs } from '../ui'

type Row = api.TrialBalanceRow

const ASSET_TYPES = ['receivable', 'bank_cash', 'current_asset', 'fixed_asset', 'asset']
const LIABILITY_TYPES = ['payable', 'current_liability', 'tax', 'liability']
const EQUITY_TYPES = ['equity']

const bal = (r: Row): number => Number(r.balance) || 0
const fmt = (n: number): string => n.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })
const sum = (rows: Row[], f: (r: Row) => number): number => rows.reduce((t, r) => t + f(r), 0)
const ofType = (rows: Row[], types: string[]): Row[] => rows.filter((r) => types.includes(String(r.account_type)))

function Num({ value, bold }: { value: number; bold?: boolean }) {
  return (
    <span className={'t-mono tabular-nums ' + (bold ? 'font-semibold text-text' : 'text-text')}>{fmt(value)}</span>
  )
}

/** A labelled section of accounts with a subtotal (used by P&L and the balance sheet). */
function Section({ title, rows, amount }: { title: string; rows: Row[]; amount: (r: Row) => number }) {
  const total = sum(rows, amount)
  return (
    <div className="mb-5">
      <div className="t-label mb-2 text-muted">{title}</div>
      <div className="divide-y divide-border rounded-md border border-border">
        {rows.length === 0 ? (
          <div className="px-3 py-2 t-body text-muted">Nothing yet.</div>
        ) : (
          rows.map((r) => (
            <div key={r.account_id} className="flex items-center justify-between px-3 py-1.5">
              <span className="t-body text-text">
                <span className="t-mono mr-2 text-muted">{r.code}</span>
                {r.name}
              </span>
              <Num value={amount(r)} />
            </div>
          ))
        )}
        <div className="flex items-center justify-between bg-surface2 px-3 py-2">
          <span className="t-label text-text">Total {title}</span>
          <Num value={total} bold />
        </div>
      </div>
    </div>
  )
}

export function Reports() {
  const [rows, setRows] = useState<Row[] | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    void api
      .reportTrialBalance()
      .then(setRows)
      .catch((e: unknown) => setError(e instanceof api.ApiError && e.status === 403 ? 'forbidden' : e instanceof Error ? e.message : 'Failed to load'))
  }, [])

  if (error === 'forbidden') {
    return (
      <div>
        <PageHeader title="Reports" subtitle="Financial statements" />
        <p className="t-caption flex items-center gap-1.5 text-muted">
          <AlertTriangle size={13} /> Viewing financial reports requires accounting access.
        </p>
      </div>
    )
  }
  if (error) return <ErrorState message={error} />

  const revenue = rows ? ofType(rows, ['income']) : []
  const expense = rows ? ofType(rows, ['expense']) : []
  const assets = rows ? ofType(rows, ASSET_TYPES) : []
  const liabilities = rows ? ofType(rows, LIABILITY_TYPES) : []
  const equity = rows ? ofType(rows, EQUITY_TYPES) : []

  const revenueTotal = sum(revenue, (r) => -bal(r))
  const expenseTotal = sum(expense, (r) => bal(r))
  const netResult = revenueTotal - expenseTotal
  const assetTotal = sum(assets, bal)
  const liabilityTotal = sum(liabilities, (r) => -bal(r))
  const equityTotal = sum(equity, (r) => -bal(r))

  const totalDebit = rows ? sum(rows, (r) => Number(r.debit) || 0) : 0
  const totalCredit = rows ? sum(rows, (r) => Number(r.credit) || 0) : 0

  const trialBalance = (
    <div className="overflow-x-auto">
      <table className="w-full text-[13px]">
        <thead>
          <tr className="border-b border-border t-label text-muted">
            <th className="px-3 py-2 text-left font-medium">Account</th>
            <th className="px-3 py-2 text-right font-medium">Debit</th>
            <th className="px-3 py-2 text-right font-medium">Credit</th>
            <th className="px-3 py-2 text-right font-medium">Balance</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-border/60">
          {(rows ?? []).map((r) => (
            <tr key={r.account_id}>
              <td className="px-3 py-1.5 text-text">
                <span className="t-mono mr-2 text-muted">{r.code}</span>
                {r.name}
              </td>
              <td className="px-3 py-1.5 text-right"><Num value={Number(r.debit) || 0} /></td>
              <td className="px-3 py-1.5 text-right"><Num value={Number(r.credit) || 0} /></td>
              <td className="px-3 py-1.5 text-right"><Num value={bal(r)} /></td>
            </tr>
          ))}
        </tbody>
        <tfoot>
          <tr className="border-t border-border bg-surface2">
            <td className="px-3 py-2 t-label text-text">Totals</td>
            <td className="px-3 py-2 text-right"><Num value={totalDebit} bold /></td>
            <td className="px-3 py-2 text-right"><Num value={totalCredit} bold /></td>
            <td className="px-3 py-2 text-right t-caption text-muted">{Math.abs(totalDebit - totalCredit) < 0.005 ? 'balanced' : 'unbalanced'}</td>
          </tr>
        </tfoot>
      </table>
    </div>
  )

  const pnl = (
    <div>
      <Section title="Revenue" rows={revenue} amount={(r) => -bal(r)} />
      <Section title="Expenses" rows={expense} amount={bal} />
      <div className="flex items-center justify-between rounded-md border border-accent/30 bg-accent-soft px-3 py-2.5">
        <span className="t-subtitle font-medium text-text">Net result</span>
        <Num value={netResult} bold />
      </div>
    </div>
  )

  const balanceSheet = (
    <div className="grid gap-5 lg:grid-cols-2">
      <div>
        <Section title="Assets" rows={assets} amount={bal} />
        <div className="flex items-center justify-between rounded-md border border-border bg-surface2 px-3 py-2.5">
          <span className="t-subtitle font-medium text-text">Total assets</span>
          <Num value={assetTotal} bold />
        </div>
      </div>
      <div>
        <Section title="Liabilities" rows={liabilities} amount={(r) => -bal(r)} />
        <Section title="Equity" rows={equity} amount={(r) => -bal(r)} />
        <div className="mb-5 flex items-center justify-between px-3 py-1.5">
          <span className="t-body text-text">Current-year result</span>
          <Num value={netResult} />
        </div>
        <div className="flex items-center justify-between rounded-md border border-border bg-surface2 px-3 py-2.5">
          <span className="t-subtitle font-medium text-text">Liabilities + equity</span>
          <Num value={liabilityTotal + equityTotal + netResult} bold />
        </div>
      </div>
    </div>
  )

  return (
    <div>
      <PageHeader title="Reports" subtitle="Posted journal entries, as financial statements" />
      {!rows ? (
        <Card className="p-5">
          <SkeletonText lines={6} />
        </Card>
      ) : (
        <Card className="p-5">
          <Tabs
            tabs={[
              { id: 'tb', label: 'Trial balance', content: trialBalance },
              { id: 'pnl', label: 'Profit & loss', content: pnl },
              { id: 'bs', label: 'Balance sheet', content: balanceSheet },
            ]}
          />
        </Card>
      )}
    </div>
  )
}
