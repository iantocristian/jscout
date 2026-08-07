export function reserveInventory(skus: string[]) {
  return inventoryClient.reserve(skus);
}
