import { platformPercent, platformRemaining } from "./dashboard"
import { dollars } from "./format"
import { QuotaMeter } from "./quota-meter"
import type { Account } from "./schemas"

export function PlatformStatus(props: { platform: NonNullable<Account["platform"]>; historical: boolean }) {
  const estimate = () => platformRemaining(props.platform.balance)
  const initial = () => props.platform.balance?.initialUSD
  const title = () => {
    const { excluded } = estimate()
    return excluded ? `${excluded} unpriced ${excluded === 1 ? "request" : "requests"} excluded` : "Estimated funds remaining through ExetRouter"
  }
  return <QuotaMeter label={estimate().excluded ? "Platform API · partial estimate" : "Platform API"}
    title={title()} meterLabel="Estimated Platform funds left"
    remaining={platformPercent(estimate().usd, initial())} historical={props.historical}>
    <span>{estimate().usd === null ? "Unknown" : dollars(estimate().usd)}{initial() === undefined ? "" : ` / ${dollars(initial())}`}</span>
  </QuotaMeter>
}
