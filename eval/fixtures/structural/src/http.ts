import { checkout } from "./index";

export function handleCheckout(request: { order: { id: string; skus: string[]; total: number } }) {
  return checkout(request.order);
}
