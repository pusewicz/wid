# frozen_string_literal: true

require_relative 'wid/version'
require_relative 'wid/parser'
require_relative 'wid/lexer'
require_relative 'wid/codegen'

module Wid
  def self.parse(input)
    lexer = Lexer.new(input)
    parser = Parser.new(lexer)
    parser.parse
  end
end
