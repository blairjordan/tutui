// Reference implementation of docs/PROTOCOL.md: synthetic traffic, no network.
const params = JSON.parse(process.env.TUTUI_PARAMS ?? "{}")
const seconds = Number(params.seconds ?? 20)
const rate = Number(params.rate ?? 30)
const emit = (event) => process.stdout.write(JSON.stringify(event) + "\n")

let stopping = false
process.stdin.on("data", (buf) => {
  if (buf.toString().includes('"stop"')) stopping = true
})

emit({
  type: "hello",
  scenario: "example",
  metrics: [
    { name: "requests", kind: "counter", description: "simulated requests" },
    { name: "latency_ms", kind: "histogram", unit: "ms", description: "simulated latency" },
    { name: "in_flight", kind: "gauge", description: "simulated concurrency" },
  ],
})

const started = Date.now()
let phase = ""
const timer = setInterval(() => {
  const elapsed = (Date.now() - started) / 1000
  const next = elapsed < seconds / 2 ? "steady" : "degraded"
  if (next !== phase) {
    phase = next
    emit({ type: "phase", name: phase })
  }
  const observations = []
  for (let i = 0; i < rate; i++) {
    const slow = phase === "degraded" && Math.random() < 0.3
    const status = phase === "degraded" && Math.random() < 0.1 ? "503" : "200"
    observations.push({ metric: "requests", value: 1, labels: { status } })
    observations.push({ metric: "latency_ms", value: (slow ? 400 : 40) + Math.random() * 60 })
  }
  observations.push({ metric: "in_flight", value: Math.round(rate / 5 + Math.random() * 4) })
  emit({ type: "batch", observations })
  if (stopping || elapsed >= seconds) {
    clearInterval(timer)
    emit({ type: "done", summary: { seconds: elapsed, stopped: stopping } })
    process.exit(0)
  }
}, 1000)
