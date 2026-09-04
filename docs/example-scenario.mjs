// Reference implementation of docs/PROTOCOL.md. Synthetic traffic, no network:
// a concurrency ramp whose latency degrades and whose queue stops draining
// past a certain stage, so thresholds and the ceiling have something to say.
const params = JSON.parse(process.env.TUTUI_PARAMS ?? "{}")
const stages = params.stages ?? [10, 25, 50, 100]
const stageSeconds = Number(params.stage_seconds ?? 20)
const capacity = Number(params.capacity ?? 60) // requests/s the imaginary backend can serve

const emit = (event) => process.stdout.write(JSON.stringify(event) + "\n")
const randn = () => (Math.random() + Math.random() + Math.random()) / 3

let stopping = false
process.stdin.on("data", (buf) => {
  if (buf.toString().includes('"stop"')) stopping = true
})

emit({
  type: "hello",
  scenario: "example",
  metrics: [
    { name: "requests", kind: "counter", description: "responses by status" },
    { name: "latency_ms", kind: "histogram", unit: "ms", description: "response time" },
    { name: "in_flight", kind: "gauge", description: "concurrent requests" },
    { name: "queue_depth", kind: "gauge", description: "requests waiting at the backend" },
    { name: "target_concurrency", kind: "gauge", description: "stage concurrency" },
  ],
})

let queue = 0
let tick = 0
let stageIndex = -1
const timer = setInterval(() => {
  const nextStage = Math.min(stages.length - 1, Math.floor(tick / stageSeconds))
  if (nextStage !== stageIndex) {
    stageIndex = nextStage
    emit({ type: "phase", name: `c=${stages[stageIndex]}` })
  }
  const concurrency = stages[stageIndex]
  const load = concurrency / capacity // >1 means the backend is saturated
  const served = Math.min(concurrency, capacity)
  queue = Math.max(0, queue + concurrency - capacity) // sawtooth: drains every 5th tick
  if (tick % 5 === 4) queue = Math.max(0, queue - capacity)

  const observations = []
  for (let i = 0; i < served; i++) {
    const base = 40 + 160 * Math.max(0, load - 0.5) * 2 // flat until half capacity, then climbs
    const tail = Math.random() < 0.05 * load ? 400 * load : 0
    const failed = load > 1 && Math.random() < 0.08 * (load - 1)
    observations.push({ metric: "requests", value: 1, labels: { status: failed ? "503" : "200" } })
    observations.push({ metric: "latency_ms", value: base + tail + randn() * 40 + queue * 3 })
  }
  observations.push({ metric: "in_flight", value: Math.round(concurrency * (0.8 + randn() * 0.4)) })
  observations.push({ metric: "queue_depth", value: queue })
  observations.push({ metric: "target_concurrency", value: concurrency })
  emit({ type: "batch", observations })

  tick++
  if (stopping || tick >= stageSeconds * stages.length) {
    clearInterval(timer)
    emit({ type: "log", level: "info", message: `finished after ${tick}s; peak queue ${queue}` })
    emit({ type: "done", summary: { ticks: tick, stopped: stopping } })
    process.exit(0)
  }
}, 1000)
