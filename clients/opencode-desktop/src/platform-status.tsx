import { Show } from "solid-js"
import { platformPercent, platformRemaining } from "./dashboard"
import { dollars } from "./format"
import { QuotaMeter } from "./quota-meter"
import type { Account } from "./schemas"

export function PlatformStatus(props: { platform: NonNullable<Account["platform"]>; historical: boolean }) {
  const remaining = () => platformRemaining(props.platform.balance)
  const initial = () => props.platform.balance?.initialUSD
  return <QuotaMeter label="Platform API" meterLabel="Estimated Platform funds left"
    remaining={platformPercent(remaining(), initial())} historical={props.historical || props.platform.refreshError}>
    <span>{dollars(remaining())}{initial() === undefined ? "" : ` / ${dollars(initial())}`}</span>
    <Show when={props.platform.refreshError}><span role="status">Platform refresh failed</span></Show>
  </QuotaMeter>
}
