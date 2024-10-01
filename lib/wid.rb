# frozen_string_literal: true

require_relative "wid/version"
require_relative "wid/parser"
require_relative "wid/lexer"
require_relative "wid/codegen"

module Wid
  def self.parse(input)
    tokens = Lexer.tokenize(input)
    Parser.parse(tokens)
  end

  def self.generate(input, debug: false)
    ast = parse(input)
    if debug
      puts "AST"
      ast.expressions.each do |node|
        puts node.inspect
      end
      puts "ENDAST"
    end
    Codegen.generate(ast)
  end
end
