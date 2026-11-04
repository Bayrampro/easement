import 'dart:convert';
import 'dart:io';

String quote(String value) => "'${value.replaceAll("'", "'\"'\"'")}'";

void main(List<String> args) {
  final config = File.fromUri(
    Platform.script.resolve('../.dart_tool/package_config.json'),
  );
  if (!config.existsSync())
    throw StateError(
      'Run dart pub get --enforce-lockfile in tools/dart-indexer first',
    );
  final source = File.fromUri(Platform.script.resolve('semantics.dart'));
  final dart = File(Platform.resolvedExecutable).resolveSymbolicLinksSync();
  final header = jsonEncode({
    'dart': dart,
    'config': config.absolute.path,
    'source': source.absolute.path,
  });
  final output = File(
    args.isEmpty
        ? Platform.script
              .resolve('../../../target/dart-indexer/easement_dart_semantics')
              .toFilePath()
        : args.single,
  );
  output.parent.createSync(recursive: true);
  if (Platform.isWindows)
    throw UnsupportedError(
      'Semantic launcher currently requires Unix; use Dart to invoke semantics.dart with its pinned --packages configuration',
    );
  output.writeAsStringSync(
    '#!/bin/sh\n# easement-dart-semantics-v1 $header\nexec ${quote(dart)} ${quote('--packages=${config.absolute.path}')} ${quote(source.absolute.path)} "\$@"\n',
  );
  final result = Process.runSync('chmod', ['+x', output.path]);
  if (result.exitCode != 0) throw StateError('${result.stderr}');
  stdout.writeln(output.absolute.path);
}
