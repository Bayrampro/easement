import 'dart:convert';
import 'dart:io';

String quote(String value) => "'${value.replaceAll("'", "'\"'\"'")}'";

void main(List<String> args) {
  if (Platform.isWindows) {
    stderr.writeln(
      'Use dart pub global activate scip_dart 1.8.1 on Windows; '
      'the pinned shell launcher supports Unix. Unknown launchers bypass caching.',
    );
    exitCode = 1;
    return;
  }
  final config = File.fromUri(
    Platform.script.resolve('../.dart_tool/package_config.json'),
  );
  final json = jsonDecode(config.readAsStringSync()) as Map<String, dynamic>;
  final package = (json['packages'] as List<dynamic>)
      .cast<Map<String, dynamic>>()
      .singleWhere((package) => package['name'] == 'scip_dart');
  final root = Uri.directory(
    Directory.fromUri(config.uri.resolve(package['rootUri'] as String))
        .absolute
        .path,
  );
  final source = root.resolve('bin/scip_dart.dart').toFilePath();
  final dart = File(Platform.resolvedExecutable).resolveSymbolicLinksSync();
  final header = jsonEncode({
    'dart': dart,
    'config': config.absolute.path,
    'source': source,
  });
  final output = File(
    args.isEmpty
        ? Platform.script
              .resolve('../../../target/dart-indexer/scip_dart')
              .toFilePath()
        : args.single,
  );
  output.parent.createSync(recursive: true);
  output.writeAsStringSync(
    '#!/bin/sh\n# easement-scip-dart-v1 $header\n'
    'exec ${quote(dart)} ${quote('--packages=${config.absolute.path}')} '
    '${quote(source)} "\$@"\n',
  );
  final result = Process.runSync('chmod', ['+x', output.path]);
  if (result.exitCode != 0)
    throw ProcessException(
      'chmod',
      ['+x', output.path],
      '${result.stderr}',
      result.exitCode,
    );
  stdout.writeln(output.absolute.path);
}
