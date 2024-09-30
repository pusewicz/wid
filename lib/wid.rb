# frozen_string_literal: true

require 'wid/version'
require 'wid/parser'
require 'wid/lexer'
require 'wid/codegen'

module Wid
  def self.parse(input)
    lexer = Lexer.new(input)
    parser = Parser.new(lexer)
    parser.parse
  end
end
