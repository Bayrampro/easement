import 'orders.dart';

class Storage implements OrderRepository {
  @override
  Future<Orders> load() async => [];
}

class LoadOrders {
  final OrderRepository repository;
  LoadOrders(this.repository);
  Future<void> run() async { await repository.load(); }
}

class OrdersViewModel {
  final OrderRepository repository;
  OrdersViewModel(this.repository);
  Future<List<Order>> refresh() async {
    await repository.load();
    return [];
  }
}

void bootstrap() { LoadOrders(Storage()); }
