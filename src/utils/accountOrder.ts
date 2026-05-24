export type AccountOrderMove = "first" | "previous" | "next" | "last";

export function areStringArraysEqual(left: string[], right: string[]): boolean {
  return (
    left.length === right.length &&
    left.every((value, index) => value === right[index])
  );
}

export function normalizeAccountOrder(
  order: string[],
  availableIds: string[],
): string[] {
  const available = new Set(availableIds);
  const seen = new Set<string>();
  const next: string[] = [];

  for (const accountId of order) {
    if (!available.has(accountId) || seen.has(accountId)) continue;
    next.push(accountId);
    seen.add(accountId);
  }

  for (const accountId of availableIds) {
    if (seen.has(accountId)) continue;
    next.push(accountId);
    seen.add(accountId);
  }

  return next;
}

export function normalizeSelectedAccountOrder(
  order: string[],
  availableIds: string[],
): string[] {
  const available = new Set(availableIds);
  const seen = new Set<string>();
  const next: string[] = [];

  for (const accountId of order) {
    if (!available.has(accountId) || seen.has(accountId)) continue;
    next.push(accountId);
    seen.add(accountId);
  }

  return next;
}

export function moveIdInOrder(
  order: string[],
  accountId: string,
  move: AccountOrderMove,
): string[] {
  const currentIndex = order.indexOf(accountId);
  if (currentIndex < 0) return order;

  let targetIndex = currentIndex;
  if (move === "first") targetIndex = 0;
  if (move === "previous") targetIndex = currentIndex - 1;
  if (move === "next") targetIndex = currentIndex + 1;
  if (move === "last") targetIndex = order.length - 1;

  if (targetIndex < 0 || targetIndex >= order.length || targetIndex === currentIndex) {
    return order;
  }

  const next = [...order];
  const [moved] = next.splice(currentIndex, 1);
  next.splice(targetIndex, 0, moved);
  return next;
}

export function moveIdsBeforeTarget(
  order: string[],
  accountIds: string[],
  targetAccountId: string,
): string[] {
  if (accountIds.includes(targetAccountId)) return order;

  const moving = new Set(accountIds);
  const movedIds = order.filter((accountId) => moving.has(accountId));
  if (movedIds.length === 0) return order;

  const remaining = order.filter((accountId) => !moving.has(accountId));
  const targetIndex = remaining.indexOf(targetAccountId);
  if (targetIndex < 0) return order;

  const next = [...remaining];
  next.splice(targetIndex, 0, ...movedIds);
  return next;
}
