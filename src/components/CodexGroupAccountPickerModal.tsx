import { useEffect, useMemo, useRef, useState } from 'react'
import { FolderPlus, LogOut, Search, X } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import type { CodexAccount } from '../types/codex'
import type { CodexAccountGroup } from '../services/codexAccountGroupService'
import { buildCodexAccountPresentation } from '../presentation/platformAccountPresentation'
import {
  compareCodexAccountTieBreak,
  compareCodexAccountsByRecommendedSort,
} from '../utils/codexAccountSort'
import './GroupAccountPickerModal.css'

interface CodexGroupAccountPickerModalProps {
  isOpen: boolean
  targetGroup: CodexAccountGroup | null
  accounts: CodexAccount[]
  accountGroups: CodexAccountGroup[]
  mode?: 'add' | 'remove'
  currentAccountId?: string | null
  maskAccountText: (value?: string | null) => string
  onClose: () => void
  onConfirm: (payload: { accountIds: string[] }) => Promise<void> | void
}

export function CodexGroupAccountPickerModal({
  isOpen,
  targetGroup,
  accounts,
  accountGroups,
  mode = 'add',
  currentAccountId,
  maskAccountText,
  onClose,
  onConfirm,
}: CodexGroupAccountPickerModalProps) {
  const { t } = useTranslation()
  const [query, setQuery] = useState('')
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState('')
  const selectAllCheckboxRef = useRef<HTMLInputElement | null>(null)

  useEffect(() => {
    if (!isOpen) return
    setQuery('')
    setSelected(new Set())
    setError('')
  }, [isOpen, targetGroup])

  const groupByAccountId = useMemo(() => {
    const result = new Map<string, CodexAccountGroup>()
    for (const group of accountGroups) {
      for (const accountId of group.accountIds) {
        if (!result.has(accountId)) {
          result.set(accountId, group)
        }
      }
    }
    return result
  }, [accountGroups])

  const visibleAccounts = useMemo(() => {
    if (!targetGroup) return []

    const queryText = query.trim().toLowerCase()
    const existingIds = new Set(targetGroup.accountIds)
    const groupOrder = new Map(
      targetGroup.accountIds.map((accountId, index) => [accountId, index]),
    )
    let next =
      mode === 'remove'
        ? accounts.filter((account) => existingIds.has(account.id))
        : accounts.filter((account) => !existingIds.has(account.id))

    next = next.sort((a, b) => {
      const recommendedDiff = compareCodexAccountsByRecommendedSort(a, b, {
        currentAccountId,
      })
      if (recommendedDiff !== 0) return recommendedDiff

      if (mode === 'remove') {
        const orderDiff =
          (groupOrder.get(a.id) ?? Number.MAX_SAFE_INTEGER) -
          (groupOrder.get(b.id) ?? Number.MAX_SAFE_INTEGER)
        if (orderDiff !== 0) return orderDiff
      }
      return compareCodexAccountTieBreak(a, b)
    })

    if (!queryText) return next

    return next.filter((account) => {
      const presentation = buildCodexAccountPresentation(account, t)
      const currentGroupName = groupByAccountId.get(account.id)?.name?.toLowerCase() || ''
      return (
        presentation.displayName.toLowerCase().includes(queryText)
        || currentGroupName.includes(queryText)
      )
    })
  }, [accounts, currentAccountId, groupByAccountId, mode, query, t, targetGroup])

  const selectedVisibleCount = useMemo(
    () =>
      visibleAccounts.reduce(
        (count, account) => count + (selected.has(account.id) ? 1 : 0),
        0,
      ),
    [selected, visibleAccounts],
  )

  const allVisibleSelected =
    visibleAccounts.length > 0 && selectedVisibleCount === visibleAccounts.length

  useEffect(() => {
    if (!selectAllCheckboxRef.current) return
    selectAllCheckboxRef.current.indeterminate =
      selectedVisibleCount > 0 && !allVisibleSelected
  }, [allVisibleSelected, selectedVisibleCount])

  const toggleSelectAllVisible = () => {
    if (saving || visibleAccounts.length === 0) return

    setSelected((prev) => {
      const next = new Set(prev)
      if (allVisibleSelected) {
        for (const account of visibleAccounts) {
          next.delete(account.id)
        }
      } else {
        for (const account of visibleAccounts) {
          next.add(account.id)
        }
      }
      return next
    })
  }

  const toggleSelect = (accountId: string) => {
    if (saving) return
    setSelected((prev) => {
      const next = new Set(prev)
      if (next.has(accountId)) {
        next.delete(accountId)
      } else {
        next.add(accountId)
      }
      return next
    })
  }

  const handleConfirm = async () => {
    if (!targetGroup || selected.size === 0 || saving) return

    setSaving(true)
    setError('')
    try {
      await onConfirm({
        accountIds: Array.from(selected),
      })
      onClose()
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setSaving(false)
    }
  }

  if (!isOpen || !targetGroup) return null
  const confirmLabel =
    mode === 'remove'
      ? t('accounts.groups.removeFromGroup')
      : t('accounts.groups.addAccounts')

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal group-account-picker-modal" onClick={(event) => event.stopPropagation()}>
        <div className="modal-header">
          <h2 className="group-account-picker-title">
            {mode === 'remove' ? <LogOut size={18} /> : <FolderPlus size={18} />}
            <span>{confirmLabel}</span>
            <span className="group-account-picker-target">{targetGroup.name}</span>
          </h2>
          <button
            className="modal-close"
            onClick={onClose}
            aria-label={t('common.close')}
          >
            <X size={18} />
          </button>
        </div>

        <div className="modal-body group-account-picker-body">
          <div className="group-account-toolbar">
            <div className="group-account-search">
              <Search size={16} className="group-account-search-icon" />
              <input
                type="text"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder={t('accounts.search')}
              />
            </div>
          </div>

          <div className="group-account-item group-account-item-header">
            <input
              ref={selectAllCheckboxRef}
              type="checkbox"
              checked={allVisibleSelected}
              onChange={toggleSelectAllVisible}
              disabled={saving || visibleAccounts.length === 0}
            />
            <div className="group-account-main" />
          </div>

          <div className="group-account-list">
            {visibleAccounts.length === 0 ? (
              <div className="group-account-empty">
                {mode === 'remove'
                  ? t('accounts.groups.accountRemovePickerEmpty', '当前分组暂无账号')
                  : t('accounts.groups.accountPickerEmpty')}
              </div>
            ) : (
              visibleAccounts.map((account) => {
                const currentGroup = groupByAccountId.get(account.id) || null
                const presentation = buildCodexAccountPresentation(account, t)
                const isChecked = selected.has(account.id)
                const isUngrouped = !currentGroup
                const isCurrentAccount = currentAccountId === account.id

                return (
                  <label
                    key={account.id}
                    className={`group-account-item${isChecked ? ' is-current' : ''}${
                      isCurrentAccount ? ' is-active-account' : ''
                    }`}
                  >
                    <input
                      type="checkbox"
                      checked={isChecked}
                      disabled={saving}
                      onChange={() => toggleSelect(account.id)}
                    />
                    <div className="group-account-main">
                      <span
                        className="group-account-email"
                        title={maskAccountText(presentation.displayName)}
                      >
                        {maskAccountText(presentation.displayName)}
                      </span>
                      <div className="group-account-meta">
                        <span className={`tier-badge ${presentation.planClass} group-account-tier-badge`}>
                          {presentation.planLabel}
                        </span>
                        {isCurrentAccount && (
                          <span className="group-account-badge is-current">
                            {t('codex.current', '当前')}
                          </span>
                        )}
                        <span className={`group-account-badge${isUngrouped ? ' is-ungrouped' : ''}`}>
                          {isUngrouped ? t('accounts.groups.ungrouped') : currentGroup.name}
                        </span>
                      </div>
                    </div>
                  </label>
                )
              })
            )}
          </div>

          {error && <div className="group-account-error">{error}</div>}
        </div>

        <div className="modal-footer group-account-picker-footer">
          <button className="btn btn-secondary" onClick={onClose} disabled={saving}>
            {t('common.cancel')}
          </button>
          <button
            className="btn btn-primary"
            onClick={handleConfirm}
            disabled={selected.size === 0 || saving}
          >
            {saving ? t('common.saving') : `${confirmLabel} (${selected.size})`}
          </button>
        </div>
      </div>
    </div>
  )
}

export default CodexGroupAccountPickerModal
