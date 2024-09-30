# frozen_string_literal: true

require_relative 'nodes'

module Wid
  class Parser
    ParserError = Class.new(StandardError)

    UnexpectedTokenError = Class.new(ParserError) do
      attr_reader :token

      def initialize(token, next_token, expected = nil, file:, line:)
        @token = token
        super("Unexpected token #{token.type} `#{token.value}' at #{file}:#{line}. Expected `#{expected || next_token.type}'")
      end
    end
    UnrecognizedTokenError = Class.new(ParserError) do
      attr_reader :token
      def initialize(token, file:, line:)
        @token = token
        super("Unrecognized token #{token.type} `#{token.value}' at #{file}:#{line}")
      end
    end

    UNARY_OPERATORS = %i[! -].freeze
    BINARY_OPERATORS = %i[+ - * / == != < <= > >=].freeze
    LOGICAL_OPERATORS = %i[&& ||].freeze

    LOWEST_PRECEDENCE = 0
    PREFIX_PRECEDENCE = 7
    OPERATOR_PRECEDENCE = {
      '||': 1,
      '&&': 2,
      '==': 3,
      '!=': 3,
      '>':  4,
      '<':  4,
      '>=': 4,
      '<=': 4,
      '+':  5,
      '-':  5,
      '*':  6,
      '/':  6,
      '(':  8
    }.freeze

    attr_reader :errors

    def initialize(tokens)
      @tokens = tokens || raise(ArgumentError, 'tokens must be provided')
      @root = Nodes::Program.new
      @pos = 0
      @errors = []
    end

    def self.parse(tokens) = new(tokens).parse

    def parse
      while @pos < @tokens.size
        consume

        node = parse_expression_recursively
        @root << node if node
      end

      @root
    end

    private

    def unrecognized_token_error(file:, line:)
      @errors << UnrecognizedTokenError.new(current, file, line, file:, line:)
    end

    def unexpected_token_error(expected = nil, file:, line:)
      @errors << UnexpectedTokenError.new(current, peek, expected, file:, line:)
    end

    def consume(offset = 1)
      tok = peek(offset)
      @pos += offset
      tok
    end

    def consume_if_next_token_is(expected)
      if peek.type == expected.type
        consume
        true
      else
        unexpected_token_error(expected, file: __FILE__, line: __LINE__)
        false
      end
    end

    def peek(offset = 1)
      pos = (@pos - 1) + offset
      return if pos < 0 || pos >= @tokens.size

      @tokens[pos]
    end

    def previous = peek(-1)
    def current = peek(0)
    def current_precedence = OPERATOR_PRECEDENCE.fetch(current.type, LOWEST_PRECEDENCE)
    def next_token_not_terminator? = peek.type != :"\n" && peek.type != :EOF
    def next_token_precedence = OPERATOR_PRECEDENCE.fetch(peek.type, LOWEST_PRECEDENCE)
    def check_syntax_compliance(node)
      return if node.expects?(peek)
      unexpected_token_error(nil, file: __FILE__, line: __LINE__)
    end

    def parse_expression_recursively(precedence = LOWEST_PRECEDENCE)
      parsing_function = determine_parsing_function

      return unrecognized_token_error(file: __FILE__, line: __LINE__) unless parsing_function

      expr = send(parsing_function)

      return unless expr

      while next_token_not_terminator? && precedence < next_token_precedence
        infix_parsing_function = determine_infix_parsing_function(peek)

        return expr unless infix_parsing_function

        consume

        expr = send(infix_parsing_function, expr)
      end

      expr
    end

    KNOWN_TOKENS = %i[IDENTIFIER NUMBER + STRING].freeze

    def determine_parsing_function
      if KNOWN_TOKENS.include?(current.type)
        "parse_#{current.type.downcase}".to_sym
      elsif current.type == :"("
        :parse_grouped_expression
      elsif [:"\n", :EOF].include?(current.type)
        :parse_terminator
      elsif UNARY_OPERATORS.include?(current.type)
        :parse_unary_operator
      end
    end

    def determine_infix_parsing_function(token = current)
      if (BINARY_OPERATORS + LOGICAL_OPERATORS).include?(token.type)
        :parse_binary_operator
      elsif token.type == :'('
        :parse_function_call
      end
    end

    def build_token(type, value = nil)
      ::Wid::Lexer::Token.new(type, value, nil, nil)
    end

    def parse_binary_operator(left)
      op = Nodes::BinaryOperator.new(current.type, left)
      op_precedence = current_precedence

      consume
      op.right = parse_expression_recursively(op_precedence)

      op
    end

    # TODO: Temporary impl; reflect more deeply about the appropriate way of parsing a terminator.
    def parse_terminator = nil
    def parse_number = Nodes::Number.new(current.value)
    def parse_identifier
      if peek.type == :'='
        parse_var_binding
      else
        ident = Nodes::Identifier.new(current.value)
        check_syntax_compliance(ident)
        ident
      end
    end

    def parse_string = Nodes::String.new(current.value)

    def parse_function_call(identifier)
      Nodes::FunctionCall.new(identifier, parse_function_call_args)
    end

    def parse_function_call_args
      args = []

      # Function call without arguments.
      if peek.type == :')'
        consume
        return args
      end

      consume
      args << parse_expression_recursively

      while peek.type == :','
        consume(2)
        args << parse_expression_recursively
      end

      return unless consume_if_next_token_is(build_token(:')', ')'))
      args
    end
  end
end
