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
})

