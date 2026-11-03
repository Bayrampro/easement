import 'dart:async';
import 'other.dart' as other;
import 'package:example/deferred.dart' deferred as lazy;
export 'other.dart';
import 'stub.dart' if (dart.library.io) 'native.dart';
Future<void> main() async { await lazy.loadLibrary(); other.Public(); print(PlatformMarker()); }
