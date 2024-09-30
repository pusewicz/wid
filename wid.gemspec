# frozen_string_literal: true

require_relative 'lib/wid/version'

Gem::Specification.new do |s|
  s.name          = 'wid'
  s.version       = Wid::VERSION
  s.summary       = 'A simple language'
  s.description   = 'Wid is a language inspired by Lisp and Ruby. It is a simple language that is easy to learn and use.'
  s.authors       = ['Piotr Usewicz']
  s.email         = 'piotr@layer22.com'
  s.homepage      = 'https://github.com/pusewicz/wid'
  s.license       = 'MIT'


  s.bindir        = 'exe'
  s.files         = `git ls-files -z`.split("\x0")
  s.test_files    = s.files.grep(%r{^test/})
  s.executables   = s.files.grep(%r{^exe/}) { |f| File.basename(f) }
  s.require_paths = ['lib']

  s.required_ruby_version = Gem::Requirement.new('>= 3.3.0')
  s.add_development_dependency('rake', '~> 13.0')
  s.add_development_dependency('tldr', '~> 0.10.0')
  s.add_development_dependency('logger')
  s.add_development_dependency('fiddle')
end
