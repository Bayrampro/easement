class Order {}
class WireOrder {}
typedef Orders = List<WireOrder?>;
abstract interface class OrderRepository {
  Future<Orders> load();
}
