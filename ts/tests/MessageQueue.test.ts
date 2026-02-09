import { describe, expect, it } from "vitest"
import { InMemoryStorageAdapter } from "../src/StorageAdapter"
import { MessageQueue } from "../src/MessageQueue"

const fixedNow = (start = 1_000_000) => {
  let t = start
  return {
    now: () => t,
    advance: (ms: number) => (t += ms),
    value: () => t,
  }
}

describe("MessageQueue", () => {
  it("enqueues per target and preserves FIFO", async () => {
    const storage = new InMemoryStorageAdapter()
    const clock = fixedNow()
    const q = new MessageQueue<string>({ storage, queueKey: "mq1", now: clock.now })

    await q.enqueueForTargets("msgA", ["t1"]) // created at t
    clock.advance(10)
    await q.enqueueForTargets("msgB", ["t2"]) // created at t+10

    const r1 = await q.reserveNext()
    expect(r1?.payload).toBe("msgA")
    const r2 = await q.reserveNext()
    // r1 is still locked (sending), second reserve returns the next available
    expect(r2?.payload).toBe("msgB")

    // Ack second then first; size should go to 0 after both acks
    await q.ack(r2!.id)
    await q.ack(r1!.id)
    expect(await q.size()).toBe(0)
  })

  it("requeues on fail with backoff", async () => {
    const storage = new InMemoryStorageAdapter()
    const clock = fixedNow()
    const q = new MessageQueue<string>({ storage, queueKey: "mq2", now: clock.now })

    await q.enqueueForTargets("hello", ["d1"])
    const first = await q.reserveNext()
    expect(first?.payload).toBe("hello")

    // Fail with 1s backoff
    await q.fail(first!.id, { nextAvailableAt: clock.value() + 1000 })

    // Should not be available immediately
    const none = await q.reserveNext()
    expect(none).toBeUndefined()

    clock.advance(1000)
    const again = await q.reserveNext()
    expect(again?.payload).toBe("hello")
    expect(again?.attempts).toBe(1)
  })

  it("supports deferred group expansion without duplicates", async () => {
    const storage = new InMemoryStorageAdapter()
    const clock = fixedNow()
    const q = new MessageQueue<string>({ storage, queueKey: "mq3", now: clock.now })

    await q.enqueueForGroup("hi group", "ownerABC")
    await q.expandGroup("ownerABC", ["dev1", "dev2"]) // create 2 sendable items
    await q.expandGroup("ownerABC", ["dev1", "dev3"]) // add dev3 only

    const all = await q.dump()
    const sendables = all.filter((it) => !it.targetGroupKey)
    expect(sendables.map((s) => s.target).sort()).toEqual(["dev1", "dev2", "dev3"]) // no duplicates
  })

  it("persists across instances", async () => {
    const storage = new InMemoryStorageAdapter()
    const clock = fixedNow()
    const q1 = new MessageQueue<string>({ storage, queueKey: "mq4", now: clock.now })
    await q1.enqueueForTargets("persist me", ["t1"])

    // New instance with same storage+key
    const q2 = new MessageQueue<string>({ storage, queueKey: "mq4", now: clock.now })
    const it = await q2.reserveNext()
    expect(it?.payload).toBe("persist me")
  })

  it("recovers stuck locks after timeout", async () => {
    const storage = new InMemoryStorageAdapter()
    const clock = fixedNow()
    const q = new MessageQueue<string>({ storage, queueKey: "mq5", now: clock.now, lockTimeoutMs: 500 })

    await q.enqueueForTargets("stuck", ["t1"])
    const reserved = await q.reserveNext()
    expect(reserved?.status).toBe("sending")

    // Lock is held - should not be available
    expect(await q.reserveNext()).toBeUndefined()

    // Advance past timeout
    clock.advance(501)
    const recovered = await q.reserveNext()
    expect(recovered?.payload).toBe("stuck")
    expect(recovered?.id).toBe(reserved!.id)
  })

  it("enqueueForTargets creates one item per target", async () => {
    const storage = new InMemoryStorageAdapter()
    const clock = fixedNow()
    const q = new MessageQueue<string>({ storage, queueKey: "mq6", now: clock.now })

    const ids = await q.enqueueForTargets("broadcast", ["d1", "d2", "d3"])
    expect(ids).toHaveLength(3)
    expect(await q.size()).toBe(3)

    const items = await q.dump()
    expect(items.map((i) => i.target).sort()).toEqual(["d1", "d2", "d3"])
    // All share the same payload
    expect(items.every((i) => i.payload === "broadcast")).toBe(true)
  })

  it("expandGroup with no matching templates is a no-op", async () => {
    const storage = new InMemoryStorageAdapter()
    const clock = fixedNow()
    const q = new MessageQueue<string>({ storage, queueKey: "mq7", now: clock.now })

    const result = await q.expandGroup("nonexistent", ["d1"])
    expect(result.created).toBe(0)
    expect(await q.size()).toBe(0)
  })

  it("ack for nonexistent item does not throw", async () => {
    const storage = new InMemoryStorageAdapter()
    const clock = fixedNow()
    const q = new MessageQueue<string>({ storage, queueKey: "mq8", now: clock.now })

    await expect(q.ack("does-not-exist")).resolves.toBeUndefined()
  })

  it("fail for nonexistent item does not throw", async () => {
    const storage = new InMemoryStorageAdapter()
    const clock = fixedNow()
    const q = new MessageQueue<string>({ storage, queueKey: "mq9", now: clock.now })

    await expect(q.fail("does-not-exist")).resolves.toBeUndefined()
  })

  it("templates are never returned by reserveNext", async () => {
    const storage = new InMemoryStorageAdapter()
    const clock = fixedNow()
    const q = new MessageQueue<string>({ storage, queueKey: "mq10", now: clock.now })

    await q.enqueueForGroup("template only", "group1")
    expect(await q.reserveNext()).toBeUndefined()
    expect(await q.size()).toBe(0)

    // But dump includes it
    const all = await q.dump()
    expect(all).toHaveLength(1)
    expect(all[0].targetGroupKey).toBe("group1")
  })

  it("expanded items inherit createdAt from template for FIFO stability", async () => {
    const storage = new InMemoryStorageAdapter()
    const clock = fixedNow()
    const q = new MessageQueue<string>({ storage, queueKey: "mq11", now: clock.now })

    // Template created at t=1000000
    await q.enqueueForGroup("early", "g1")
    clock.advance(5000)
    // Concrete item created at t=1005000
    await q.enqueueForTargets("later", ["d1"])
    // Expand template at t=1005000 - items should keep original createdAt
    await q.expandGroup("g1", ["d2"])

    // Reserve should return the template-expanded item first (earlier createdAt)
    const first = await q.reserveNext()
    expect(first?.payload).toBe("early")
    expect(first?.target).toBe("d2")

    const second = await q.reserveNext()
    expect(second?.payload).toBe("later")
    expect(second?.target).toBe("d1")
  })

  it("multiple group templates expand independently", async () => {
    const storage = new InMemoryStorageAdapter()
    const clock = fixedNow()
    const q = new MessageQueue<string>({ storage, queueKey: "mq12", now: clock.now })

    await q.enqueueForGroup("for alice", "alice")
    await q.enqueueForGroup("for bob", "bob")

    await q.expandGroup("alice", ["a-dev1", "a-dev2"])
    await q.expandGroup("bob", ["b-dev1"])

    expect(await q.size()).toBe(3)
    const items = await q.dump()
    const sendables = items.filter((i) => !i.targetGroupKey)
    expect(sendables.map((s) => `${s.payload}:${s.target}`).sort()).toEqual([
      "for alice:a-dev1",
      "for alice:a-dev2",
      "for bob:b-dev1",
    ])
  })

  it("fail without nextAvailableAt makes item immediately available", async () => {
    const storage = new InMemoryStorageAdapter()
    const clock = fixedNow()
    const q = new MessageQueue<string>({ storage, queueKey: "mq13", now: clock.now })

    await q.enqueueForTargets("retry", ["d1"])
    const item = await q.reserveNext()
    await q.fail(item!.id) // no backoff

    const again = await q.reserveNext()
    expect(again?.payload).toBe("retry")
    expect(again?.attempts).toBe(1)
  })

  it("enqueueForTargets with delayMs defers availability", async () => {
    const storage = new InMemoryStorageAdapter()
    const clock = fixedNow()
    const q = new MessageQueue<string>({ storage, queueKey: "mq14", now: clock.now })

    await q.enqueueForTargets("delayed", ["d1"], { delayMs: 2000 })
    expect(await q.reserveNext()).toBeUndefined()

    clock.advance(2000)
    const item = await q.reserveNext()
    expect(item?.payload).toBe("delayed")
  })

  it("size excludes templates", async () => {
    const storage = new InMemoryStorageAdapter()
    const clock = fixedNow()
    const q = new MessageQueue<string>({ storage, queueKey: "mq15", now: clock.now })

    await q.enqueueForGroup("tmpl", "g1")
    await q.enqueueForTargets("concrete", ["d1"])

    expect(await q.size()).toBe(1)
    // dump includes both
    expect((await q.dump()).length).toBe(2)
  })
})

