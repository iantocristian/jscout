import { reserveInventory } from "./inventory";
import { authorizePayment } from "./payments";

export function checkout(order: { id: string; skus: string[]; total: number }) {
  reserveInventory(order.skus);
  authorizePayment(order.id, order.total);
  orderBus.emit("order.created", order.id);
  return order.id;
}
