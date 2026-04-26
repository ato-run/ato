import { Hono } from 'hono'

const app = new Hono()

app.get('/', (c) => {
  return c.text('Hello form Capsule + Hono! 🚀')
})

export default {
  // 環境変数 PORT があればそれを使い、なければ 3000 を使う
  port: process.env.PORT ? parseInt(process.env.PORT) : 3000, 
  fetch: app.fetch,
}