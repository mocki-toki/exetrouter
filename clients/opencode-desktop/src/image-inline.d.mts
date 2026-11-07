import type { Context } from "@opencode/plugin/gui"
export const instructions: string
export const enabled: (tools: unknown) => boolean
export function installInlineImages(ctx: Context, sdk: Record<string, unknown>): boolean
export function createCard(options: any): any
export function findCard(messages: any[], token: string, collect: any): any
export function markers(text: string): string[]
export function renderCard(root: any, state: any, refresh: any, document: any): void
export const css: string
