// Integration tests use the pinned analyzer and a real prepared Dart package.
import 'dart:convert';
import 'dart:io';

void expect(bool value, String message) {
  if (!value) throw StateError(message);
}

Future<void> main() async {
  final package = File.fromUri(Platform.script.resolve('../pubspec.yaml'))
      .parent;
  final config = package.uri
      .resolve('.dart_tool/package_config.json')
      .toFilePath();
  final extractor = package.uri.resolve('bin/semantics.dart').toFilePath();
  final temp = Directory.systemTemp.createTempSync('easement-semantics-test-');
  try {
    File(
      '${temp.path}/pubspec.yaml',
    ).writeAsStringSync('name: semantic_test\nenvironment:\n  sdk: ^3.13.0\n');
    Directory('${temp.path}/lib').createSync();
    final source = File('${temp.path}/lib/orders.dart');
    source.writeAsStringSync('''
class WireOrder {}
class Order {}
typedef Orders = List<WireOrder?>;
abstract class Base<T> { T inherited(); }
abstract interface class Repository extends Base<Orders> {
  Future<Orders> load();
  abstract WireOrder field;
  void record((WireOrder, {WireOrder named}) input);
  void callback(void Function(WireOrder) input);
}
class Implementation { void run() { final inferred = WireOrder(); print(inferred); } }
class Recursive<T extends Comparable<T>> { T value; Recursive(this.value); }
''');
    final pub = Process.runSync(Platform.resolvedExecutable, [
      'pub',
      'get',
      '--offline',
    ], workingDirectory: temp.path);
    expect(pub.exitCode == 0, 'fixture preparation failed: ${pub.stderr}');
    Future<Map<String, dynamic>> run() async {
      final process = await Process.start(Platform.resolvedExecutable, [
        '--packages=$config',
        extractor,
      ]);
      process.stdin.write(
        jsonEncode({
          'root': temp.path,
          'package': temp.path,
          'sdk': File(Platform.resolvedExecutable).parent.parent.path,
          'files': ['lib/orders.dart'],
        }),
      );
      await process.stdin.close();
      final outputs = await Future.wait([
        process.stdout.transform(utf8.decoder).join(),
        process.stderr.transform(utf8.decoder).join(),
      ]);
      expect(await process.exitCode == 0, outputs[1]);
      return jsonDecode(outputs[0]) as Map<String, dynamic>;
    }

    final facts = await run();
    expect(facts['format_version'] == 2, 'fact version');
    expect(
      (facts['gaps'] as List).isEmpty,
      'valid package is partial: ${facts['gaps']}',
    );
    final declarations = (facts['declarations'] as List)
        .cast<Map<String, dynamic>>();
    final repository = declarations.singleWhere(
      (d) => d['identity']['name'] == 'Repository',
    );
    expect(
      (repository['modifiers'] as List).contains('interface'),
      'missing interface modifier',
    );
    expect(
      repository['identity']['library'] == 'package:semantic_test/orders.dart',
      'unstable library identity',
    );
    final api = (repository['api'] as List).cast<Map<String, dynamic>>();
    expect(
      api.where((m) => m['member'] == 'field').length == 1,
      'synthetic getter/setter duplicated one public field',
    );
    final inherited = api.singleWhere((m) => m['member'] == 'inherited');
    expect(
      jsonEncode(inherited['type']).contains('WireOrder'),
      'generic substitution missing',
    );
    expect(inherited['origin']['line'] == 4, 'inherited origin missing');
    final load = api.singleWhere((m) => m['member'] == 'load');
    expect(
      jsonEncode(load['type']).contains('"alias":true'),
      'alias provenance missing',
    );
    expect(load['location']['line'] == 6, 'signature location missing');
    expect(load['operation'] == 'return', 'signature slot missing');
    expect(
      jsonEncode(
        api.singleWhere(
          (m) =>
              m['member'] == 'record' &&
              m['operation'].toString().startsWith('parameter:'),
        )['type'],
      ).contains('WireOrder'),
      'record facts missing',
    );
    expect(
      jsonEncode(
        api.singleWhere(
          (m) =>
              m['member'] == 'callback' &&
              m['operation'].toString().startsWith('parameter:'),
        )['type'],
      ).contains('WireOrder'),
      'callback facts missing',
    );
    final implementation = declarations.singleWhere(
      (d) => d['identity']['name'] == 'Implementation',
    );
    expect(
      jsonEncode(implementation['uses']).contains('"operation":"variable"'),
      'inferred local facts missing',
    );
    source.writeAsStringSync(
      '${source.readAsStringSync()}\nclass Broken { Missing value; }\n',
    );
    final broken = await run();
    expect((broken['gaps'] as List).isNotEmpty, 'analysis errors lost');
    final version = Process.runSync(Platform.resolvedExecutable, [
      '--packages=$config',
      extractor,
      '--version',
    ]);
    expect(
      version.stdout.toString().contains('facts=2 analyzer=14.4.0'),
      'version handshake missing',
    );
    stdout.writeln('Dart semantic extraction tests passed');
  } finally {
    temp.deleteSync(recursive: true);
  }
}
