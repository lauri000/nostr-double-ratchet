import { StorageAdapter } from "./StorageAdapter"

export type QueueStatus = "pending" | "sending" | "sent" | "failed"

export interface QueueItem<T> {
  id: string
  createdAt: number
  availableAt?: number
  attempts: number
  status: QueueStatus
  payload: T
  /** Single concrete target (e.g., device/pubkey) */
  target?: string
  /** Template/grouping key for deferred target expansion (e.g., owner pubkey) */
  targetGroupKey?: string
  /** Internal lock for reservations */
  lockId?: string
  lockedAt?: number
  /** Template items track which targets have already been expanded */
  expandedTargets?: string[]
}

export interface EnqueueOptions {
  /** Timestamp provider (ms) */
  now?: () => number
  /** Delay before the item becomes available (ms) */
  delayMs?: number
}

export interface FailOptions {
  error?: unknown
  /** Set next available time (ms since epoch). If omitted, item becomes available immediately. */
  nextAvailableAt?: number
}

export interface MessageQueueOptions {
  /** Namespace key for the queue under the adapter */
  queueKey: string
  /** Storage adapter for persistence */
  storage: StorageAdapter
  /** Prefix used for keys inside the adapter (defaults to v1/queue) */
  keyPrefix?: string
  /** Timestamp provider (ms) */
  now?: () => number
  /** Lock timeout (ms) to recover stuck reservations */
  lockTimeoutMs?: number
}

/**
 * A simple FIFO persisted message queue with per-recipient entries and
 * support for deferred recipient expansion via group templates.
 */
export class MessageQueue<T> {
  private readonly storage: StorageAdapter
  private readonly queueKey: string
  private readonly prefix: string
  private readonly now: () => number
  private readonly lockTimeoutMs: number

  constructor(opts: MessageQueueOptions) {
    this.storage = opts.storage
    this.queueKey = opts.queueKey
    this.prefix = (opts.keyPrefix ?? "v1/queue") + "/" + this.queueKey
    this.now = opts.now ?? (() => Date.now())
    this.lockTimeoutMs = opts.lockTimeoutMs ?? 60_000
  }

  // -------------
  // Public API
  // -------------

  /** Enqueue one item per concrete target. */
  async enqueueForTargets(payload: T, targets: string[], opt: EnqueueOptions = {}): Promise<string[]> {
    const ids: string[] = []
    const createdAt = opt.now ? opt.now() : this.now()
    const availableAt = opt.delayMs ? createdAt + opt.delayMs : undefined
    for (const target of targets) {
      const item = this.buildItem(payload, { createdAt, availableAt, target })
      await this.putItem(item)
      ids.push(item.id)
    }
    return ids
  }

  /**
   * Enqueue a template item referencing a group key whose concrete targets
   * are not yet known. Call expandGroup later to fan-out per-target entries.
   */
  async enqueueForGroup(payload: T, groupKey: string, opt: EnqueueOptions = {}): Promise<string> {
    const createdAt = opt.now ? opt.now() : this.now()
    const availableAt = opt.delayMs ? createdAt + opt.delayMs : undefined
    const item = this.buildItem(payload, {
      createdAt,
      availableAt,
      targetGroupKey: groupKey,
    })
    // Mark as a template by having targetGroupKey and no concrete target
    item.expandedTargets = []
    await this.putItem(item)
    return item.id
  }

  /**
   * Expand all template items for a given group key into concrete per-target entries.
   * Avoids duplicates across repeated expansions.
   */
  async expandGroup(groupKey: string, targets: string[]): Promise<{ created: number }> {
    const templateItems = await this.findItems((it) => !!it.targetGroupKey && it.targetGroupKey === groupKey)
    let created = 0
    for (const tmpl of templateItems) {
      const expanded = new Set(tmpl.expandedTargets ?? [])
      const newTargets = targets.filter((t) => !expanded.has(t))
      if (newTargets.length === 0) continue

      for (const t of newTargets) {
        const clone: QueueItem<T> = {
          id: this.makeId(),
          createdAt: tmpl.createdAt,
          availableAt: Math.max(this.now(), tmpl.availableAt ?? 0) || undefined,
          attempts: 0,
          status: "pending",
          payload: tmpl.payload,
          target: t,
        }
        await this.putItem(clone)
        created++
      }

      tmpl.expandedTargets = Array.from(new Set([...(tmpl.expandedTargets ?? []), ...newTargets]))
      // Keep template for future devices; do not transition status
      await this.putItem(tmpl)
    }
    return { created }
  }

  /** Reserve the next available item (FIFO by createdAt). */
  async reserveNext(): Promise<QueueItem<T> | undefined> {
    const items = await this.listItems()
    // Filter to pending or expired locks
    const now = this.now()
    const candidates = items
      .filter((it) => !it.targetGroupKey) // templates are never sendable
      .filter((it) => (it.availableAt ?? 0) <= now)
      .filter((it) => this.isAvailableToLock(it, now))
      .sort((a, b) => a.createdAt - b.createdAt)

    const next = candidates[0]
    if (!next) return undefined

    // Lock it
    next.status = "sending"
    next.lockId = this.makeId()
    next.lockedAt = now
    await this.putItem(next)
    return next
  }

  /** Acknowledge success; deletes the item by default. */
  async ack(itemId: string): Promise<void> {
    await this.delItem(itemId)
  }

  /**
   * Mark a reservation as failed; re-queue as pending and increment attempts.
   * Optionally set a future availability time for backoff.
   */
  async fail(itemId: string, opt: FailOptions = {}): Promise<void> {
    const item = await this.getItem(itemId)
    if (!item) return
    item.status = "pending"
    item.attempts += 1
    item.lockId = undefined
    item.lockedAt = undefined
    if (opt.nextAvailableAt !== undefined) item.availableAt = opt.nextAvailableAt
    await this.putItem(item)
  }

  /** Number of concrete (sendable) items still pending or locked. */
  async size(): Promise<number> {
    const items = await this.listItems()
    return items.filter((it) => !it.targetGroupKey).length
  }

  /** Peek all items (including templates). Useful for tests/debug. */
  async dump(): Promise<QueueItem<T>[]> {
    return this.listItems()
  }

  // -------------
  // Internals
  // -------------

  private itemsPrefix(): string {
    return `${this.prefix}/items/`
  }

  private itemKey(item: QueueItem<T>): string {
    // Use createdAt ordering in key for natural lexicographic sort
    const ts = String(item.createdAt).padStart(16, "0")
    return `${this.itemsPrefix()}${ts}-${item.id}`
  }

  private async listItemKeys(): Promise<string[]> {
    const keys = await this.storage.list(this.itemsPrefix())
    // Always sort by key to achieve FIFO by createdAt
    keys.sort()
    return keys
  }

  private async listItems(): Promise<QueueItem<T>[]> {
    const keys = await this.listItemKeys()
    const items = await Promise.all(keys.map((k) => this.storage.get<QueueItem<T>>(k)))
    return items.filter(Boolean) as QueueItem<T>[]
  }

  private async findItems(predicate: (it: QueueItem<T>) => boolean): Promise<QueueItem<T>[]> {
    const items = await this.listItems()
    return items.filter(predicate)
  }

  private async getItem(id: string): Promise<QueueItem<T> | undefined> {
    const items = await this.listItems()
    return items.find((it) => it.id === id)
  }

  private async putItem(item: QueueItem<T>): Promise<void> {
    await this.storage.put(this.itemKey(item), item)
  }

  private async delItem(id: string): Promise<void> {
    const keys = await this.listItemKeys()
    for (const k of keys) {
      const it = await this.storage.get<QueueItem<T>>(k)
      if (it?.id === id) {
        await this.storage.del(k)
        return
      }
    }
  }

  private isAvailableToLock(item: QueueItem<T>, now: number): boolean {
    if (item.status === "pending") return true
    if (item.status !== "sending") return false
    // Recover if locked too long
    if (!item.lockedAt) return true
    return now - item.lockedAt > this.lockTimeoutMs
  }

  private makeId(): string {
    // Non-crypto unique id; sufficient for queue identity
    return `${Math.random().toString(36).slice(2)}${Math.random().toString(36).slice(2)}`
  }

  private buildItem(
    payload: T,
    p: { createdAt: number; availableAt?: number; target?: string; targetGroupKey?: string }
  ): QueueItem<T> {
    return {
      id: this.makeId(),
      createdAt: p.createdAt,
      availableAt: p.availableAt,
      attempts: 0,
      status: "pending",
      payload,
      target: p.target,
      targetGroupKey: p.targetGroupKey,
    }
  }
}

