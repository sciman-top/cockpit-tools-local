import { Rows3 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { SingleSelectFilterDropdown } from './SingleSelectFilterDropdown';

interface PaginationControlsProps {
  totalItems: number;
  currentPage: number;
  totalPages: number;
  pageSize: number;
  pageSizeOptions: readonly number[];
  rangeStart: number;
  rangeEnd: number;
  canGoPrevious: boolean;
  canGoNext: boolean;
  onPageSizeChange: (pageSize: number) => void;
  onPageChange: (page: number) => void;
  onPreviousPage: () => void;
  onNextPage: () => void;
}

export function PaginationControls({
  totalItems,
  currentPage,
  totalPages,
  pageSize,
  pageSizeOptions,
  rangeStart,
  rangeEnd,
  canGoPrevious,
  canGoNext,
  onPageSizeChange,
  onPageChange,
  onPreviousPage,
  onNextPage,
}: PaginationControlsProps) {
  const { t } = useTranslation();

  if (totalItems === 0) {
    return null;
  }

  return (
    <div className="pagination-container">
      <div className="pagination-info">
        {t('pagination.info', {
          start: rangeStart,
          end: rangeEnd,
          total: totalItems,
          defaultValue: 'Showing {{start}} - {{end}} of {{total}}',
        })}
      </div>
      <div className="pagination-controls">
        <SingleSelectFilterDropdown
          value={String(pageSize)}
          options={pageSizeOptions.map((count) => ({
            value: String(count),
            label: t('pagination.perPage', {
              count,
              defaultValue: '{{count}} / page',
            }),
          }))}
          ariaLabel={t('pagination.perPage', {
            count: pageSize,
            defaultValue: '{{count}} / page',
          })}
          icon={<Rows3 size={14} />}
          onChange={(value) => onPageSizeChange(Number.parseInt(value, 10))}
        />
        <div className="pagination-buttons">
          <button
            type="button"
            className="pagination-btn"
            onClick={onPreviousPage}
            disabled={!canGoPrevious}
          >
            {t('pagination.prev', 'Previous')}
          </button>
          <label className="pagination-page-jump">
            <span>{t('pagination.pagePrefix', 'Page')}</span>
            <select
              className="pagination-page-select"
              value={currentPage}
              onChange={(event) => onPageChange(Number.parseInt(event.target.value, 10))}
              aria-label={t('pagination.jumpToPage', 'Jump to page')}
            >
              {Array.from({ length: totalPages }, (_, index) => index + 1).map((page) => (
                <option key={page} value={page}>
                  {page}
                </option>
              ))}
            </select>
            <span>/ {totalPages}</span>
          </label>
          <button
            type="button"
            className="pagination-btn"
            onClick={onNextPage}
            disabled={!canGoNext}
          >
            {t('pagination.next', 'Next')}
          </button>
        </div>
      </div>
    </div>
  );
}
