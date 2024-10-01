# frozen_string_literal: true

require 'strscan'

module Wid
  class Lexer
    Token = Data.define(:type, :value, :line, :column)

    IDENTIFIER = /[_A-Za-z][_0-9A-Za-z]*\b/
    WHITESPACE = /[ \r\t]+/
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

    def self.tokenize(input)
      new(input).tokenize
    end

    def initialize(input)
      @scanner = StringScanner.new(input)
      @last_pos = 0
      @line_number = 0
    end

    def tokenize
      tokens = []

      while (tok = next_token)
        tokens << tok
      end

      tokens << token(:EOF)
    end

    def column_number
      @scanner.pos - @last_pos - @scanner.matched_size.to_i
    end

    def next_token
      @scanner.skip(WHITESPACE)

      return if @scanner.eos?

      tok = if (s = @scanner.scan(PUNCTUATION))
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

      if @scanner.matched == "\n"
        @last_pos = @scanner.pos
        @line_number += 1
      end

      tok
    end

    private

    def token(type, value = nil)
      Token.new(type: type, value: value, line: @line_number, column: column_number)
    end
  end
end
