import { Show } from "solid-js"
import { platformRemaining } from "./dashboard"
import { dollars } from "./format"
import type { Account } from "./schemas"

export function PlatformStatus(props: { platform: NonNullable<Account["platform"]> }) {
  const balance = () => props.platform.balance
  const estimate = () => platformRemaining(balance())
  const remaining = () => estimate().usd
  const initial = () => balance()?.initialUSD
  const ratio = () => {
    const usd = remaining(), total = initial()
    return usd != null && total != null && total > 0 ? Math.max(0, Math.min(100, usd / total * 100)) : undefined
  }
  return <div class="exr-quota" data-tone="neutral">
    <div class="exr-inline"><span title={estimate().excluded ? `${estimate().excluded} unpriced ${estimate().excluded === 1 ? "request" : "requests"} excluded` : undefined}>Platform API · {estimate().excluded ? "partial estimate" : "estimated left"}</span><strong>{remaining() == null ? "Unknown" : dollars(remaining()!)}{initial() != null ? ` / ${dollars(initial()!)}` : ""}</strong></div>
    <div class="exr-quota-track" role={ratio() === undefined ? "img" : "meter"} aria-label="Estimated Platform funds left" aria-valuemin={0} aria-valuemax={100} aria-valuenow={ratio()}>
      <Show when={ratio() !== undefined}><span style={{ width: `${ratio()}%` }} /></Show>
    </div>
  </div>
}
