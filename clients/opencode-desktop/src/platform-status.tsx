import { Show } from "solid-js"
import { dollars } from "./format"
import type { Account } from "./schemas"

export function PlatformStatus(props: { platform: NonNullable<Account["platform"]> }) {
  const balance = () => props.platform.balance
  const remaining = () => balance()?.exetrouter.remainingUSD
  const initial = () => balance()?.initialUSD
  const ratio = () => {
    const usd = remaining(), total = initial()
    return usd != null && total != null && total > 0 ? Math.max(0, Math.min(100, usd / total * 100)) : undefined
  }
  return <div class="exr-quota" data-tone="neutral">
    <div class="exr-inline"><span>Platform API · estimated left</span><strong>{remaining() == null ? "Unknown" : dollars(remaining()!)}{initial() != null ? ` / ${dollars(initial()!)}` : ""}</strong></div>
    <div class="exr-quota-track" role={ratio() === undefined ? "img" : "meter"} aria-label="Estimated Platform funds left" aria-valuemin={0} aria-valuemax={100} aria-valuenow={ratio()}>
      <Show when={ratio() !== undefined}><span style={{ width: `${ratio()}%` }} /></Show>
    </div>
  </div>
}
