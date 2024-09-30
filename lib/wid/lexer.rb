# frozen_string_literal: true

require 'strscan'

module Wid
  class Lexer
    Token = Data.define(:type, :value, :line, :column)

    IDENTIFIER = /[_A-Za-z][_0-9A-Za-z]*\b/
    WHITESPACE = /[ \r\t]/
    INTEGER = /-?(?:0|[1-9][0-9]*)/
    FLOAT_DECIMAL = /[.][0-9]+/
    NUMBER =  /#{INTEGER}(#{FLOAT_DECIMAL})?/
    STRING = /"([^"\\]|\\.)*"/
    # STRING =/"[^"]*"/

    KEYWORDS = %w[true false nil def end].freeze

    KW_RE = /#{Regexp.union(KEYWORDS.sort)}\b/
    KW_TABLE = Hash[KEYWORDS.map { |kw| [kw, kw.upcase.to_sym] }]

    LITERALS = %W[{ } ( ) [ ] = ! | & + - \n].freeze
    DOT = '.'

    PUNCTUATION = Regexp.union(LITERALS)
    PUNCTUATION_TABLE = LITERALS.map { |x| [x, x.to_sym] }.to_h

    attr_reader :start

    def self.tokenize(input)
      new(input).tokenize
    end

    def initialize(input)
      @input = input
      @scanner = StringScanner.new(input)
      @line = 0
      @column = 0
    end

    def tokenize
      tokens = []

      while (tok = next_token)
        tokens << tok
      end

      tokens << token(:EOF)
    end

    def next_token
      @scanner.skip(WHITESPACE)

      return if @scanner.eos?

      tok = if (s = @scanner.scan(PUNCTUATION))
        @line += 1 if s == "\n"
        token(PUNCTUATION_TABLE[s], s)
      elsif (s = @scanner.scan(KW_RE))
        token(KW_TABLE[s], s)
      elsif (s = @scanner.scan(STRING))
        token(:STRING, s)
      elsif (s = @scanner.scan(IDENTIFIER))
        token(:IDENTIFIER, s)
      elsif (s = @scanner.scan(NUMBER))
        token(:NUMBER, s)
      elsif (s = @scanner.scan(DOT))
        token(:DOT)
      else
        token(:UNKNOWN, @scanner.getch)
      end

      @column += @scanner.matched_size

      tok
    end

    private

    def token(type, value = nil)
      # FIXME: Column calculation is incorrect
      Token.new(type: type, value: value, line: @line, column: @column - @scanner.pos)
    end
  end
end
