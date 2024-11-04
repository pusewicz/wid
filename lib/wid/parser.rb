# frozen_string_literal: true

require_relative "ast/node"

module Wid
  class Parser
    class StandardError < ::StandardError; end

    class ParseError < StandardError; end

    def initialize(tokens)
      @tokens = tokens
      @current = 0
    end

    def self.parse(tokens)
      new(tokens).parse
    end

    def parse
      statements = []

      statements << expression until eof?

      AST::ProgramNode.new(statements: AST::StatementsNode.new(body: statements))
    end

    # print_statement → "print" expression ( "," expression )* ;
    def print_statement
      expressions = []

      # Parse first expression (required)
      expressions << expression

      # Parse additional expressions separated by commas
      expressions << expression while match(:comma)

      consume(:"\n", "Expected newline after print statement.") if check(:"\n")

      AST::PrintNode.new(expressions:)
    end

    # expression → assignment ;
    def expression
      return print_statement if match(:PRINT)

      assignment
    end

    # assignment → IDENTIFIER "=" assignment | equality ;
    def assignment
      receiver = equality

      if match(:"=")
        equals = previous
        value = assignment

        if receiver.is_a?(AST::CallNode)
          return AST::LocalVariableWriteNode.new(name: receiver.name, value: value)
        end

        error(equals, "Invalid assignment target.")
      end

      receiver
    end

    # equality → comparison ( ( "!=" | "==" ) comparison )* ;
    def equality
      receiver = comparison

      while match(:"!=", :"==")
        name = previous.type
        arguments = AST::ArgumentsNode.new(arguments: [comparison])
        receiver = AST::CallNode.new(receiver:, name:, arguments:, block: nil)
      end

      receiver
    end

    # comparison → term ( ( ">" | ">=" | "<" | "<=" ) term )* ;
    def comparison
      receiver = term

      while match(:>, :">=", :<, :"<=")
        name = previous.type
        arguments = AST::ArgumentsNode.new(arguments: [term])
        receiver = AST::CallNode.new(receiver:, name:, arguments:, block: nil)
      end

      receiver
    end

    # term → factor ( ( "-" | "+" ) factor )* ;
    def term
      receiver = factor

      while match(:-, :+)
        name = previous.type
        arguments = AST::ArgumentsNode.new(arguments: [factor])
        receiver = AST::CallNode.new(receiver:, name:, arguments:, block: nil)
      end

      receiver
    end

    # factor → unary ( ( "/" | "*" ) unary )* ;
    def factor
      receiver = unary

      while match(:/, :*)
        name = previous.type
        arguments = AST::ArgumentsNode.new(arguments: [unary])
        receiver = AST::CallNode.new(receiver:, name:, arguments:, block: nil)
      end

      receiver
    end

    # unary → ( "!" | "-" ) unary | primary ;
    def unary
      if match(:!, :-)
        operator = previous.type
        right = unary

        return AST::UnaryNode.new(operator:, right:)
      end

      primary
    end

    # primary → NUMBER | STRING | "true" | "false" | "nil" | "(" expression ")" | IDENTIFIER ;
    def primary
      return number_literal if match(:NUMBER)
      return string_literal if match(:STRING)
      return bool_literal if match(:BOOL)
      return nil_literal if match(:NIL)
      return identifier if match(:IDENTIFIER)

      if match(:"(")
        expr = expression
        consume(:")", "Expect ')' after expression.")

        return AST::GroupingNode.new(expression: expr)
      end

      error peek, "Expected a primary expression, got #{peek.to_a.inspect}."
    end

    def number_literal
      if previous.value.include?(".")
        ::Wid::AST::FloatNode.new(value: Float(previous.value))
      else
        ::Wid::AST::IntegerNode.new(value: Integer(previous.value))
      end
    end

    def string_literal
      ::Wid::AST::StringNode.new(unescaped: previous.value[1...-1])
    end

    def bool_literal
      if previous.value == "true"
        ::Wid::AST::TrueNode.new
      else
        ::Wid::AST::FalseNode.new
      end
    end

    def nil_literal
      ::Wid::AST::NilNode.new
    end

    def identifier
      ::Wid::AST::CallNode.new(receiver: nil, name: previous.value.to_sym, arguments: nil, block: nil)
    end

    private

    def match(*types)
      types.each do |type|
        if check(type)
          advance
          return true
        end
      end

      false
    end

    def check(type)
      return false if eof?

      peek.type == type
    end

    def advance
      @current += 1 unless eof?

      previous
    end

    def eof?
      peek.nil?
    end

    def peek
      @tokens[@current]
    end

    def previous
      @tokens[@current - 1]
    end

    def consume(type, message)
      return advance if check(type)
      raise error(peek, message)
    end

    def error(token, message)
      raise ParseError.new(message)
    end
  end
end
